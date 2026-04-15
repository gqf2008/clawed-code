//! `/branch [name]` — fork the current conversation into a new session.
//!
//! Creates a copy of the current transcript with a new session ID,
//! preserving the full conversation history while allowing divergent exploration.

use std::fmt::Write as _;

use clawed_agent::engine::QueryEngine;
use clawed_core::session;

/// Handle `/branch [name]` command.
pub(crate) async fn handle_branch(engine: &QueryEngine, name: &str) {
    println!("{}", handle_branch_str(engine, name).await);
}

pub(crate) async fn handle_branch_str(engine: &QueryEngine, name: &str) -> String {
    let session_id = engine.session_id();
    let fork_name = if name.is_empty() { None } else { Some(name) };

    let mut out = format!("\x1b[2mForking session {}...\x1b[0m\n", &session_id[..8]);

    match session::fork_session(session_id, fork_name) {
        Ok(new_id) => {
            let _ = writeln!(
                out,
                "\x1b[32m✓\x1b[0m Forked to new session: {}",
                &new_id[..8]
            );
            let _ = writeln!(out, "  Original: {}", &session_id[..8]);

            if let Ok(entries) = session::load_transcript(&new_id) {
                let msg_count = entries
                    .iter()
                    .filter(|entry| {
                        matches!(
                            entry,
                            session::TranscriptEntry::User { .. }
                                | session::TranscriptEntry::Assistant { .. }
                        )
                    })
                    .count();
                let _ = writeln!(out, "  Messages: {}", msg_count);
            }

            if let Some(fork_name) = fork_name {
                let _ = writeln!(out, "  Name:     {}", fork_name);
            }

            let _ = write!(
                out,
                "\n\x1b[2mTo resume this fork later: /session load {}\x1b[0m",
                &new_id[..8]
            );
        }
        Err(error) => {
            let _ = write!(out, "\x1b[31mFailed to fork session: {}\x1b[0m", error);
        }
    }

    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn branch_module_exists() {
        // Integration tests for /branch are in commands.rs tests.
    }
}
