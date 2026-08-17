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
# Two maintainer-only targets rebuild the bundled data. They are the ONLY
# things here that touch the network, and users never run them:
#
#   make data       - regenerate data/wordlist.txt and data/cues.txt
#   make data-cues  - just the cue table (~20 min, streams ~39 GB)
#
# Everything in data/ is generated, committed, and embedded at compile time, so
# a build — and an install from crates.io — needs no network and no downloads.
# That is the whole point of committing a megabyte of text: the expensive
# derivation happens once, here, and every user gets the answer.
#
# Outside those targets there is no network access and no model download at any
# stage: the lexicon is a local SQLite database and the dictionary is bundled.
#
# Note: this machine's cargo came via Homebrew's keg-only rustup and may not be
# on PATH. Either add it (see CLAUDE.md) or run, e.g.:
#   make build CARGO=/opt/homebrew/opt/rustup/bin/cargo

CARGO ?= cargo
BIN   := vocab
CLAUDE_BIN_DIR ?= $(HOME)/.claude/bin

.DEFAULT_GOAL := help
.PHONY: help build release link unlink install uninstall test lint fmt clean data data-words data-cues data-cue-counts

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
	@echo ""
	@echo "maintainer only (network, slow, output is committed):"
	@echo "  make data       regenerate data/wordlist.txt and data/cues.txt"
	@echo "  make data-words SCOWL word list only"
	@echo "  make data-cues  re-derive cues from committed counts (seconds)"
	@echo "  make data-cue-counts  refetch counts from Google Books (~39 GB)"

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

# --- maintainer-only: regenerate the bundled data -------------------------
#
# Order matters. The cue builder filters context words against the word list,
# so a stale list quietly drops cues.

data: data-words data-cues

data-words:
	./script/build-wordlist.sh

# Re-derive cues from the committed counts. Seconds, no network.
data-cues:
	python3 script/derive-cues.py data/cue-counts.tsv data/cue-sets.txt \
	    data/wordlist.txt data/cues.txt 2000 150 24

# Rebuild the counts themselves from Google Books. ~39 GB, and only needed
# when the confusion sets change.
data-cue-counts:
	./script/build-cues.sh
