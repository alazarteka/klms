.PHONY: check build

check:
	cargo fmt --all -- --check
	cargo test --locked --all-targets
	cargo clippy --locked --all-targets -- -D warnings

build:
	cargo build --release --locked
