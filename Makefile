.PHONY: check build supply-chain

check: supply-chain
	./scripts/test-supply-chain.sh
	cargo fmt --all -- --check
	cargo test --locked --all-targets
	cargo clippy --locked --all-targets -- -D warnings

build:
	cargo build --release --locked

supply-chain:
	./scripts/supply-chain.sh deny
	./scripts/supply-chain.sh vet
