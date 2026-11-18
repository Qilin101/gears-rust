#!/usr/bin/env python3
# Created: 2026-11-18 by Constructor Tech
"""Record real Anthropic replies for the conformance cases.

Sends each case's anthropic_request.json to the Messages API as is and saves
the raw reply next to it: reply.json for the sync call, stream_reply.json and
stream_reply.sse for the streaming call. See tests/README.md.

    python tests/record.py [--case SUBSTRING] [--mode sync|stream] [--force] [--dry-run]

Needs the packages in tests/requirements.txt and ANTHROPIC_API_KEY; honours ANTHROPIC_BASE_URL.
"""

from __future__ import annotations

import argparse
import datetime
import json
import os
import sys
from pathlib import Path

import anthropic

CASES = Path(__file__).parent / "cases"
DEFAULT_ANTHROPIC_VERSION = "2026-10-21"
# Reply headers worth keeping; everything else (organisation ids, rate limits) stays out of git.
KEPT_HEADERS = ("content-type", "request-id", "retry-after", "retry-after-ms")
MODES = ("sync", "stream")


def load(path: Path):
    return json.loads(path.read_text()) if path.is_file() else None


def dump(path: Path, value) -> None:
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n")


def send(client: anthropic.Anthropic, request: dict, headers: dict, stream: bool):
    """POST the request as is; returns (status, headers, body bytes)."""
    try:
        # `extra_body` is merged over the typed parameters, so the body on the
        # wire is exactly `request` plus the stream flag.
        with client.messages.with_streaming_response.create(
            model=request["model"],
            max_tokens=request["max_tokens"],
            messages=request["messages"],
            extra_body=request,
            extra_headers=headers,
            timeout=600,
            **({"stream": True} if stream else {}),
        ) as response:
            return response.status_code, response.headers, response.read()
    except anthropic.APIStatusError as error:
        return error.response.status_code, error.response.headers, error.response.content


def reply_meta(status: int, headers) -> dict:
    return {
        "status": status,
        "headers": {name: headers[name] for name in KEPT_HEADERS if name in headers},
    }


def record(case: Path, args) -> str:
    config = load(case / "config.json") or {}
    request = load(case / "anthropic_request.json")
    if request is None:
        return "skipped: rejected before sending"
    if config.get("synthetic"):
        return "skipped: synthetic"

    recording = load(case / "recording.json")
    stale = recording is None or recording.get("request") != request
    files = {"sync": case / "reply.json", "stream": case / "stream_reply.json"}
    modes = [
        mode
        for mode in config.get("modes", MODES)
        if mode in args.mode and (args.force or stale or not files[mode].is_file())
    ]
    if not modes:
        return "up to date"
    if args.dry_run:
        return f"would record {', '.join(modes)}"

    options = config.get("record", {})
    settings = config.get("settings", {})
    headers = {"anthropic-version": settings.get("anthropic_version") or DEFAULT_ANTHROPIC_VERSION}
    if settings.get("anthropic_beta"):
        headers["anthropic-beta"] = ",".join(settings["anthropic_beta"])
    client = anthropic.Anthropic(max_retries=0, **({"api_key": options["api_key"]} if "api_key" in options else {}))

    notes = []
    for mode in modes:
        stream = mode == "stream"
        if options.get("warmup"):
            send(client, request, headers, stream)
        status, reply_headers, body = send(client, request, headers, stream)
        meta = reply_meta(status, reply_headers)
        sse = case / "stream_reply.sse"
        if stream and "text/event-stream" in reply_headers.get("content-type", ""):
            sse.write_bytes(body)
        else:
            sse.unlink(missing_ok=True)
            try:
                meta["body"] = json.loads(body)
            except ValueError:
                meta["body"] = body.decode("utf-8", errors="replace")
        dump(files[mode], meta)
        notes.append(f"{mode} {status}")
        if status >= 400 and not options.get("expect_error"):
            message = meta["body"].get("error", {}).get("message") if isinstance(meta["body"], dict) else meta["body"]
            notes.append(f"!! REJECTED: {message}")

    dump(
        case / "recording.json",
        {
            "recorded_at": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
            "sdk_version": anthropic.__version__,
            "request": request,
        },
    )
    return ", ".join(notes)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--case", default="", help="only cases whose path contains this")
    parser.add_argument("--mode", choices=MODES, action="append", help="only this mode (default: both)")
    parser.add_argument("--force", action="store_true", help="re-record replies that are up to date")
    parser.add_argument("--dry-run", action="store_true", help="show what would be recorded")
    args = parser.parse_args()
    args.mode = args.mode or list(MODES)

    if not args.dry_run and not (os.environ.get("ANTHROPIC_API_KEY") or os.environ.get("ANTHROPIC_AUTH_TOKEN")):
        print("set ANTHROPIC_API_KEY to record", file=sys.stderr)
        return 2

    rejected = 0
    for request in sorted(CASES.rglob("request.json")):
        case = request.parent
        name = case.relative_to(CASES).as_posix()
        if args.case not in name:
            continue
        outcome = record(case, args)
        rejected += "REJECTED" in outcome
        print(f"{name}: {outcome}")
    if rejected:
        print(f"\n{rejected} case(s) were rejected by the API: the expected request is wrong", file=sys.stderr)
    return 1 if rejected else 0


if __name__ == "__main__":
    sys.exit(main())
