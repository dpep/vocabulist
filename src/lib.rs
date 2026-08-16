//! Vocabulist — a live personal dictionary.
//!
//! The lexicon is learned from the words you actually use, with established
//! dictionaries as a backstop rather than the authority. See `docs/PLAN.md`
//! for the design.

pub mod check;
pub mod cli;
pub mod complexity;
pub mod contraction;
pub mod cue;
pub mod dict;
pub mod eval;
pub mod frequency;
pub mod help;
pub mod hook;
pub mod identity;
pub mod inbound;
pub mod ingest;
pub mod names;
pub mod ngram;
pub mod output;
pub mod process;
pub mod profile;
pub mod prune;
pub mod seed;
pub mod store;
pub mod sync;
pub mod text;
pub mod types;
pub mod watermark;
