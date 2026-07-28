.PHONY: test check clippy fmt dylint

test:
	cargo test -p cf-gears-model-registry -p cf-gears-model-registry-sdk

clippy:
	cargo clippy -p cf-gears-model-registry -p cf-gears-model-registry-sdk

fmt:
	cargo fmt -p cf-gears-model-registry -p cf-gears-model-registry-sdk

dylint:
	cargo dylint --all -p cf-gears-model-registry -p cf-gears-model-registry-sdk

check: fmt clippy test dylint
