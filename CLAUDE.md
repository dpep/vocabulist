# vocabulist development conventions

`vocabulist` is a local-first CLI that maintains a personal lexicon — the words
you actually use — and checks text against it, with an ordinary dictionary as a
backstop rather than the authority. Read [README.md](README.md) for the pitch
and [docs/PLAN.md](docs/PLAN.md) for the design contract and roadmap.
[docs/RESEARCH.md](docs/RESEARCH.md) holds the prior art behind those
decisions — reach for it before inventing a method that has a name.

> The plan doc is the contract — keep it in sync with the code, changing it in
> the same commit when the design changes.

## First principles (do not drift from these)

- **Reluctance is the product.** A false "misspelled" teaches the user to
  ignore the tool; a missed typo costs almost nothing. When a change trades
  precision for recall, it's probably wrong. New heuristics should default to
  *accept*.
- **Provenance ratchets, never downgrades.** A stronger source upgrades a word;
  a weaker sighting must never weaken one. Recurrence can lift an `observed`
  word but never past a deliberate source.
- **Capture is a spool, not an archive.** Text is staged, processed into
  counts, and dropped. Anything that retains prose past processing needs an
  explicit reason and a bound — see the exemplar cap for the shape that's
  acceptable.
- **Registers stay split.** Never sum counts across registers into a single
  frequency. Prompts and prose are different voices and averaging them produces
  a voice that exists nowhere.
- **Counting over models.** Association measures over n-gram counts are
  deterministic, explainable, and need no download. Reach for embeddings only
  where counting provably cannot work (semantic substitution), and keep the
  default path offline.
- **Only the user's own text shapes voice.** Text written by anyone else — a
  colleague, an assistant — corroborates that a *word* is real and stops
  there: no register counts, no collocations, no exemplars, no prose stats.
  Vocabulary and voice are separate axes, and conflating them feeds the
  assistant's diction back as the user's. Any new capture path goes through
  the watermark check.
- **stdout is for data, stderr is for logs.** All logging goes through
  `env_logger` to stderr so a consumer piping `vocab` gets clean output.
- **Every command is agent/script-friendly.** *All* output honors the format —
  analysis and command status alike. Resolve it once via `Cli::format()`;
  render through `output::`. `-j/--json` is a pretty array or object,
  `-J/--ndjson` is one compact object per line. When you add a command or a
  payload field, give it structured output in the same change and keep field
  names in `src/types.rs` stable — consumers parse them.
- **Exit codes follow lint convention.** Clean input exits 0, findings exit 1,
  so `vocab` drops into a hook or CI step without a wrapper.

## Language and toolchain

Rust, single static binary. `rusqlite` (bundled SQLite, WAL) for storage,
`clap` for the CLI, `serde_json` for the structured contract. No network
dependencies, and no model at runtime.

This machine's Rust came via Homebrew's keg-only `rustup`, so `cargo` may not
be on `PATH`. Either add it once —

```sh
echo 'export PATH="/opt/homebrew/opt/rustup/bin:$PATH"' >> ~/.bash_profile
```

— or invoke directly: `/opt/homebrew/opt/rustup/bin/cargo`.

## Repo layout

```text
vocabulist/
  Cargo.toml
  src/
    main.rs       ← thin entry → cli::run()
    lib.rs        ← module wiring
    cli.rs        ← Cli/Format, input resolution, dispatch, spool processing
    types.rs      ← Provenance, Register, Finding (the serialized contract)
    output.rs     ← render each payload per format
    store.rs      ← SQLite schema, lexicon/ngram/spool/exemplar DAO
    seed.rs       ← ground-truth mining (repos, taps, binaries, manifests)
    check.rs      ← the checker, suggestion ranking, bounded edit distance
    ngram.rs      ← collocations, log-likelihood, real-word confusion sets
    dict.rs       ← system word list + inflection folding (the backstop)
    frequency.rs  ← embedded core list + how common a word is in English
    text.rs       ← tokenizing, masking, and the shared `prose_words` pipeline
    contraction.rs← apostrophe-less contractions (`dont` → `don't`)
    complexity.rs ← vocabulary and readability metrics
    watermark.rs  ← assistant-authored detection
    hook.rs       ← Claude Code hook handlers
    inbound.rs    ← the user's own messages, parsed out of tool responses
    identity.rs   ← which handles are the user's, detected not configured
    ingest.rs     ← bulk load from JSON on stdin
    sync.rs       ← export into cSpell and the macOS dictionary
    eval.rs       ← labeled-corpus measurement (inject typos, score)
    profile.rs    ← --profile timings and counters
  docs/PLAN.md    ← design contract + roadmap
  docs/RESEARCH.md← prior art, methods, and candidate data sources
  tests/          ← CLI e2e harness
```

Keep it a single crate until there's a concrete reason to split.

## Building, testing, linting

```sh
make build      # dev build → target/debug/vocab
make test       # unit + e2e
make lint       # fmt --check + clippy (warnings = errors)
make fmt        # format — run before committing
```

Before committing: `cargo fmt && cargo clippy --all-targets -- -D warnings &&
cargo test`.

## Testing conventions

- Write tests for new code, focused on quality not quantity — edge cases and
  error handling over restating the happy path.
- **Verify through `cargo test`, not by hand-running the binary.** CLI behavior
  lives in `tests/cli_e2e.rs`, which drives the built binary
  (`CARGO_BIN_EXE_vocab`) against an isolated database in a temp dir.
- Give the checker a **complete** fixture dictionary. A too-small word list
  produces findings for ordinary words and the test fails for the wrong reason.
- Use generic, non-identifying test data (`zblorg`, `widget`, `Foo`). This is a
  public repo — never commit real corpus content, and never commit a lexicon.
- Spec descriptions stay simple and resilient ("raises an error", not a brittle
  exact-string match).

## Schema changes

`store.rs` owns the schema and `SCHEMA_VERSION`. New *tables* come from the
`CREATE TABLE IF NOT EXISTS` block; new *columns* need an explicit ALTER in
`migrate`, because the create block silently won't add them to an existing
database. Bump `SCHEMA_VERSION` and note the change in
[docs/PLAN.md](docs/PLAN.md) in the same commit.

## Landing changes

Solo project — commit directly to `main` and push. Keep changes small, focused,
and logically connected; change behavior or structure, not both at once. Make
sure CI is green before pushing.
