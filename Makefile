# vocabulist — build / install / test helpers.
#
#   make            - same as `make help`
#   make build      - dev build   → ./target/debug/vocab
#   make release    - optimized   → ./target/release/vocab
#   make install    - cargo install --path . (→ ~/.cargo/bin)
#   make uninstall  - cargo uninstall vocabulist
#   make test       - cargo test
#   make lint       - cargo fmt --check && cargo clippy (warnings = errors)
#   make fmt        - cargo fmt
#   make clean      - cargo clean
#
# No network access and no model download at any stage: the lexicon is a local
# SQLite database and the backstop is the system word list.
#
# Note: this machine's cargo came via Homebrew's keg-only rustup and may not be
# on PATH. Either add it (see CLAUDE.md) or run, e.g.:
#   make build CARGO=/opt/homebrew/opt/rustup/bin/cargo

CARGO ?= cargo
BIN   := vocab

.DEFAULT_GOAL := help
.PHONY: help build release install uninstall test lint fmt clean

help:
	@echo "vocabulist targets:"
	@echo "  make build      dev build → target/debug/$(BIN)"
	@echo "  make release    optimized build → target/release/$(BIN)"
	@echo "  make install    cargo install --path . (→ ~/.cargo/bin)"
	@echo "  make uninstall  cargo uninstall vocabulist"
	@echo "  make test       cargo test"
	@echo "  make lint       cargo fmt --check && cargo clippy"
	@echo "  make fmt        cargo fmt"
	@echo "  make clean      cargo clean"

build:
	$(CARGO) build

release:
	$(CARGO) build --release

install:
	$(CARGO) install --path .

uninstall:
	$(CARGO) uninstall vocabulist

test:
	$(CARGO) test

lint:
	$(CARGO) fmt --check
	$(CARGO) clippy --all-targets -- -D warnings

fmt:
	$(CARGO) fmt

clean:
	$(CARGO) clean
