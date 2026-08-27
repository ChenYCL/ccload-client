pub mod admin;
pub mod channel_usage;
pub mod channel_writer;
pub mod cli_advanced;
pub mod cli_backup;
pub mod cli_backup_diff;
pub mod cli_config;
pub mod cli_dotenv;
pub mod cli_extensions;
pub mod cli_grok;
pub mod cli_io;
pub mod cli_types;
pub mod context_window;
pub mod embed_proxy;
pub mod fallback;
pub mod forced_route;
pub mod graph;
pub mod image_mcp;
pub mod kernel;
pub mod mcp_usage;
pub mod model_import;
pub mod session_preset;
pub mod session_rescue;
pub mod system_inject;
pub mod update;
pub mod vision_mcp;

#[cfg(test)]
mod cli_tests;
