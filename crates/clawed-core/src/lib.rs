//! Core types and utilities shared across the claude-code-rs workspace.
//!
//! This crate provides the foundational building blocks — messages, tools,
//! permissions, configuration, skills, session persistence, and token
//! estimation — used by every higher-level crate.

pub mod agents;
pub mod bash_classifier;
pub mod claude_md;
pub mod concurrent_sessions;
pub mod config;
pub mod cron;
pub mod cron_lock;
pub mod cron_tasks;
pub mod file_history;
pub mod file_watcher;
pub mod git_util;
pub mod image;
pub mod memory;
pub mod message;
pub mod message_sanitize;
pub mod model;
pub mod permissions;
pub mod plugin;
pub mod session;
pub mod skills;
pub mod text_util;
pub mod token_estimation;
pub mod tool;
pub mod write_queue;
