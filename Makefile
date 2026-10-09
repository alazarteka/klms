PREFIX ?= $(HOME)/.local
BINDIR ?= $(PREFIX)/bin

.PHONY: check install-local

check:
	cargo fmt --all -- --check
	cargo test --locked --all-targets
	cargo clippy --locked --all-targets -- -D warnings

install-local:
	cargo build --release --locked
	target/release/klms __install --destination "$(BINDIR)/klms"
