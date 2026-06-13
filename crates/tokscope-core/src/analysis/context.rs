//! Context-bloat attribution — the v0.2 headline feature (CLAUDE.md §2.2, §11).
//!
//! TODO(scope): break down what fills the context window by source — system
//! prompt, per-MCP-server tool definitions, plugin/skill listings, history,
//! tool results — using first-request `cache_creation` sizes, `attachment`
//! lines (deferred_tools_delta / skill_listing) and per-event content sizes
//! already captured on the model (`Event::content_chars`, `ToolCall::server`).
