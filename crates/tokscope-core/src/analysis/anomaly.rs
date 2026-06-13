//! Retry-storm and fat-tail detection (CLAUDE.md §6 "Fat tail", roadmap v0.3).
//!
//! TODO(scope): find the small fraction of turns/requests that dominate spend
//! (p95+ outliers, rapid same-prompt retries, `isApiErrorMessage` bursts) and
//! surface them as a ranked list.
