.PHONY: test check clippy fmt

test:
	cargo test -p cf-gears-model-registry -p cf-gears-model-registry-sdk

clippy:
	cargo clippy -p cf-gears-model-registry -p cf-gears-model-registry-sdk --all-targets

fmt:
	cargo fmt -p cf-gears-model-registry -p cf-gears-model-registry-sdk

check: fmt clippy test dylint

build:
	cargo build -p cf-gears-model-registry -p cf-gears-model-registry-sdk

