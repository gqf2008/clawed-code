//! Agent engine: query loop, tool execution, compaction, and coordination.
//!
//! This crate drives the main agent loop — it receives user messages, streams
//! API responses, dispatches tool calls through the executor, handles
//! auto-compaction, and supports multi-agent coordination.

pub mod audit;
pub mod bus_adapter;
pub mod compact;
pub mod context;
pub mod coordinator;
pub mod cost;
pub mod cron_scheduler;
pub mod dispatch_agent;
pub mod engine;
pub mod executor;
pub mod hooks;
pub mod memory_extractor;
pub mod permissions;
pub mod plugin;
pub mod query;
pub mod state;
pub mod system_prompt;
pub mod system_reminder;
pub mod task_runner;
pub mod tool_result_storage;
pub mod traits;
