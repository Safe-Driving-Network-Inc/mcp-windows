pub mod cancellation;
pub mod child_process;
pub mod duration_parser;
pub mod elicitation;
pub mod event_pipe;
pub mod execution_logger;
pub mod expression_eval;
pub mod helpers;
pub mod log_pipe;
pub mod mcp_types;
pub mod omniparser;
pub mod output_parser;
#[cfg(feature = "posthog")]
pub mod posthog;
// Kareenos (fork): with the `posthog` feature off, the same entry points do nothing.
#[cfg(not(feature = "posthog"))]
pub mod posthog {
    pub fn capture(_event: &str, _properties: serde_json::Value) {}
    pub fn track_startup() {}
    pub fn track_tool_execution(_tool_name: &str, _success: bool, _duration_ms: u64, _error: Option<&str>) {}
    pub fn track_workflow_execution(_total_steps: usize, _success: bool, _duration_ms: u64) {}
}
pub mod prompt;
pub mod scripting_engine;
pub mod sentry;
pub mod server;
pub mod server_sequence;
pub mod telemetry;
pub mod tool_logging;
pub mod tools;
pub mod transpiler;
pub mod tree_formatter;
pub mod utils;
pub mod vision;
pub mod workflow_typescript;

// Re-export ui_tree_diff from terminator crate (single source of truth)
pub use terminator::ui_tree_diff;

// Re-export window_manager from terminator crate (single source of truth)
pub use terminator::{WindowCache, WindowInfo, WindowManager, WindowPlacement};

// Re-export the extract_content_json function for testing
pub use server::extract_content_json;
