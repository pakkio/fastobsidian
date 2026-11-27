//! fastobsidian: a fast explorer and editor for an Obsidian vault.
//!
//! The same code serves the app window, `--serve` (local server for any browser),
//! `-o FILE` (self-contained read-only page) and `--mcp` (MCP server on stdio).
pub mod cache;
pub mod mcp;
pub mod ops;
pub mod page;
pub mod server;
pub mod vault;
