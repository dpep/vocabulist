//! Vocabulist — a live personal dictionary.
//!
//! The lexicon is learned from the words you actually use, with established
//! dictionaries as a backstop rather than the authority. See `docs/PLAN.md`
//! for the design.

pub mod check;
pub mod cli;
pub mod dict;
pub mod ngram;
pub mod output;
pub mod profile;
pub mod seed;
pub mod store;
pub mod text;
pub mod types;
pub mod watermark;
