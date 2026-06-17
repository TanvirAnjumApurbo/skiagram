//! skiagram-core — agent-agnostic domain model, adapters, and token accounting.
//!
//! This crate is pure: no terminal I/O, no global state, no network. All file
//! access is read-only. The binary crate (`skiagram`) owns rendering and the TUI.
//!
//! Critical correctness rules live in CLAUDE.md §8; the load-bearing one is
//! request-level deduplication in [`analysis::dedup`].

pub mod adapters;
pub mod analysis;
pub mod error;
pub mod model;
pub mod pricing;
