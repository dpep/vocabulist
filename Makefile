# vocabulist — build / install / test helpers.
#
#   make            - same as `make help`
#   make build      - dev build   → ./target/debug/vocab
#   make release    - optimized   → ./target/release/vocab
#   make link       - symlink ~/.claude/bin/vocab → target/release/vocab
#   make unlink     - remove that symlink
#   make install    - cargo install --path . (→ ~/.cargo/bin)
#   make uninstall  - cargo uninstall vocabulist
#
# `link` vs `install`: a symlink tracks the build, so `make release` is
# immediately live — which is what you want while iterating, and what keeps
# the Claude Code plugin from running a binary older than the source. `install`
# copies, so it snapshots a version and needs re-running after every change.
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
CLAUDE_BIN_DIR ?= $(HOME)/.claude/bin

.DEFAULT_GOAL := help
.PHONY: help build release link unlink install uninstall test lint fmt clean

help:
	@echo "vocabulist targets:"
	@echo "  make build      dev build → target/debug/$(BIN)"
	@echo "  make release    optimized build → target/release/$(BIN)"
	@echo "  make link       symlink $(CLAUDE_BIN_DIR)/$(BIN) → target/release/$(BIN)"
	@echo "  make unlink     remove that symlink"
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

link: release
	@mkdir -p $(CLAUDE_BIN_DIR)
	@ln -sf "$(CURDIR)/target/release/$(BIN)" $(CLAUDE_BIN_DIR)/$(BIN)
	@echo "linked $(CLAUDE_BIN_DIR)/$(BIN) -> target/release/$(BIN)"
	@command -v $(BIN) >/dev/null 2>&1 || { \
	    echo ""; \
	    echo "  ⚠️  $(CLAUDE_BIN_DIR) is not on PATH — add to your shell rc:"; \
	    echo "      export PATH=\"$(CLAUDE_BIN_DIR):\$$PATH\""; \
	}

unlink:
	@rm -f $(CLAUDE_BIN_DIR)/$(BIN)
	@echo "removed $(CLAUDE_BIN_DIR)/$(BIN)"

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
