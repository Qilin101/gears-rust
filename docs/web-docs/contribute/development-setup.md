---
title: Development setup
description: Prerequisites, cloning with submodules, and building the Gears framework locally.
sidebar:
  label: Development setup
  order: 2
---

Set up a local environment to build, run, and test the framework.

## Prerequisites

- **Rust stable** with Cargo (Edition 2024, MSRV 1.95.0).
- **Protocol Buffers compiler** (`protoc`) — see the repository `README.md`.
- **Git** for version control.
- **An editor** — VS Code with rust-analyzer is recommended.

## Clone and build

```bash
# Clone gears-rust repository
git clone https://github.com/constructorfabric/gears-rust
cd gears-rust

# Install Rust if needed
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup component add clippy rustfmt

# Build and test
make build
make test
```

## Run the server locally

```bash
# SQLite quickstart (minimal runtime)
make quickstart

# With the example users_info gear
cargo run --bin cf-gears-example-server --features users-info-example -- --config config/quickstart.yaml run
```

For running and exploring the server in depth, see [Install and run](../../build-with-gears/).

## Helpful environment variables

```bash
export RUST_LOG=debug        # debug-level logging
export RUST_BACKTRACE=full   # backtraces on panic
```

## Next

- [Code contribution guide](../code-contribution-guide/) — the branch/commit/PR workflow.
- [Architecture and quality gates](../architecture-and-quality-gates/) — the checks to run before pushing.
