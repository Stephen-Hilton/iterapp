//! iter_local — everything `iter` does against the checkout on disk, with no
//! queue, server or engine involved: the structureV2 marker scan (`markers`),
//! testgroup blocks (`testgroups`), the deterministic test runner
//! (`runtests`), `validate`, and the `{placeholder}` expansion they share.
//! Ported from the V2 crate on 2026-09-04 so the V3 engine's `iter` CLI can
//! serve `runtests` / `validate` / `markers` / `teststate` / `usecase` itself
//! (the V2 binary and its `.iter/config.iter.json` are no longer needed).
//!
//! A project here is just a topdir + its main.iter.md: see `project`.

pub mod markers;
pub mod placeholders;
pub mod project;
pub mod runtests;
pub mod testgroups;
pub mod validate;

/// UTC now as RFC 3339 seconds — the timestamp shape testgroup blocks carry.
pub fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// V2 compatibility shim for the ported modules (`crate::workitems::now_iso`).
pub mod workitems {
    pub use super::now_iso;
}
