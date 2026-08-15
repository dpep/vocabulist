//! Run instrumentation for `--profile`.
//!
//! Optimization needs numbers, not intuition. This records phase timings and
//! work counters so the expensive part of a run is visible rather than
//! guessed at — the candidate-scan counter in particular, which is what makes
//! the cost of suggestion generation obvious.
//!
//! Output goes to **stderr**, never stdout: a profiled run must still pipe
//! cleanly into `jq`. Counters use interior mutability so instrumentation can
//! read `&Profile` from anywhere without threading `&mut` through call sites
//! that have no other reason to be mutable.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::Write;
use std::time::{Duration, Instant};

use crate::cli::Format;

/// Phase timings and work counters for one run.
pub struct Profile {
    enabled: bool,
    start: Instant,
    timers: RefCell<BTreeMap<&'static str, Duration>>,
    counters: RefCell<BTreeMap<&'static str, u64>>,
}

impl Profile {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            start: Instant::now(),
            timers: RefCell::new(BTreeMap::new()),
            counters: RefCell::new(BTreeMap::new()),
        }
    }

    /// A profile that records nothing — the default everywhere instrumentation
    /// is optional.
    pub fn disabled() -> Self {
        Self::new(false)
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Add `n` to a counter. A no-op when disabled, so hot-path call sites
    /// cost a branch.
    pub fn count(&self, key: &'static str, n: u64) {
        if !self.enabled {
            return;
        }
        *self.counters.borrow_mut().entry(key).or_insert(0) += n;
    }

    /// Time `f` under `key`, accumulating across calls. Runs `f` either way —
    /// only the bookkeeping is conditional.
    pub fn time<T>(&self, key: &'static str, f: impl FnOnce() -> T) -> T {
        if !self.enabled {
            return f();
        }
        let started = Instant::now();
        let out = f();
        *self
            .timers
            .borrow_mut()
            .entry(key)
            .or_insert(Duration::ZERO) += started.elapsed();
        out
    }

    pub fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }

    /// Write the report to `out` (stderr) in the run's format. Silent when
    /// profiling is off.
    pub fn report(&self, out: &mut impl Write, format: Format) -> std::io::Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let timers = self.timers.borrow();
        let counters = self.counters.borrow();
        let total = self.elapsed();

        match format {
            Format::Human => {
                writeln!(out, "profile")?;
                for (key, dur) in timers.iter() {
                    writeln!(out, "  {:<22} {:>10.2}ms", key, dur.as_secs_f64() * 1000.0)?;
                }
                if !counters.is_empty() {
                    writeln!(out)?;
                    for (key, n) in counters.iter() {
                        writeln!(out, "  {key:<22} {n:>10}")?;
                    }
                }
                writeln!(
                    out,
                    "\n  {:<22} {:>10.2}ms",
                    "total",
                    total.as_secs_f64() * 1000.0
                )
            }
            Format::Json | Format::Ndjson => {
                let timers_ms: BTreeMap<_, _> = timers
                    .iter()
                    .map(|(k, v)| (*k, v.as_secs_f64() * 1000.0))
                    .collect();
                let payload = serde_json::json!({
                    "profile": {
                        "timers_ms": timers_ms,
                        "counters": *counters,
                        "total_ms": total.as_secs_f64() * 1000.0,
                    }
                });
                let text = if format == Format::Json {
                    serde_json::to_string_pretty(&payload).unwrap()
                } else {
                    serde_json::to_string(&payload).unwrap()
                };
                writeln!(out, "{text}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_profile_records_nothing() {
        let p = Profile::disabled();
        p.count("words", 5);
        let mut buf = Vec::new();
        p.report(&mut buf, Format::Human).unwrap();
        assert!(buf.is_empty());
    }

    #[test]
    fn disabled_profile_still_runs_the_timed_work() {
        let p = Profile::disabled();
        assert_eq!(p.time("phase", || 42), 42);
    }

    #[test]
    fn counters_accumulate_across_calls() {
        let p = Profile::new(true);
        p.count("candidates", 3);
        p.count("candidates", 4);
        let mut buf = Vec::new();
        p.report(&mut buf, Format::Json).unwrap();
        let text = String::from_utf8(buf).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["profile"]["counters"]["candidates"], 7);
    }

    #[test]
    fn json_report_carries_timers_and_total() {
        let p = Profile::new(true);
        p.time("load", || {});
        let mut buf = Vec::new();
        p.report(&mut buf, Format::Json).unwrap();
        let parsed: serde_json::Value =
            serde_json::from_str(&String::from_utf8(buf).unwrap()).unwrap();
        assert!(parsed["profile"]["timers_ms"]["load"].is_number());
        assert!(parsed["profile"]["total_ms"].is_number());
    }

    #[test]
    fn human_report_names_each_phase() {
        let p = Profile::new(true);
        p.time("dictionary_load", || {});
        p.count("words", 2);
        let mut buf = Vec::new();
        p.report(&mut buf, Format::Human).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert!(text.contains("dictionary_load"));
        assert!(text.contains("words"));
        assert!(text.contains("total"));
    }
}
