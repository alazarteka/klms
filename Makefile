PREFIX ?= $(HOME)/.local
BINDIR ?= $(PREFIX)/bin

.PHONY: check install-local install-skill test

check:
	cargo fmt --all -- --check
	cargo test --locked
	cargo clippy --locked --all-targets -- -D warnings

test:
	cargo test --locked

install-local:
	cargo build --release --locked
	install -d "$(BINDIR)"
	install -m 0755 target/release/klms "$(BINDIR)/klms"
	"$(BINDIR)/klms" skill install

install-skill:
	"$(BINDIR)/klms" skill install
