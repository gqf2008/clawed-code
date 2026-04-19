//! History — conversation history manipulation for QueryEngine.

use clawed_core::message::{ContentBlock, Message};

use super::QueryEngine;

impl QueryEngine {
    /// Clear conversation history and reset token counters.
    pub async fn clear_history(&self) {
        let mut s = self.state.write().await;
        s.messages.clear();
        s.turn_count = 0;
        s.total_input_tokens = 0;
        s.total_output_tokens = 0;
    }

    /// Get the last user message text from conversation history (for /retry).
    ///
    /// Returns `None` if no user messages exist.
    pub async fn last_user_prompt(&self) -> Option<String> {
        let s = self.state.read().await;
        s.messages.iter().rev().find_map(|msg| {
            if let Message::User(u) = msg {
                u.content.iter().find_map(|b| {
                    if let ContentBlock::Text { text } = b {
                        Some(text.clone())
                    } else {
                        None
                    }
                })
            } else {
                None
            }
        })
    }

    /// Undo the last assistant turn and return the user prompt that preceded it.
    ///
    /// Removes the entire last turn — including the assistant message, any
    /// intermediate tool-result user messages, and the original user prompt —
    /// from history.  Used by `/retry` to resend the last user prompt.
    ///
    /// A "turn" may consist of more than a simple user→assistant pair when
    /// the assistant issued tool calls: the sequence can be
    /// `User(prompt) → Assistant(tool_use) → User(tool_result) → Assistant(final)`.
    /// This method correctly identifies the turn boundaries and removes the
    /// whole block.
    pub async fn pop_last_turn(&self) -> Option<String> {
        let mut s = self.state.write().await;

        // Find the last assistant message.
        let assistant_idx = s
            .messages
            .iter()
            .rposition(|m| matches!(m, Message::Assistant(_)))?;

        // Walk backwards from that assistant to find the user message that
        // starts this turn.  We skip user messages that consist solely of
        // tool results — those are intermediate responses, not the original
        // prompt.
        let user_idx = s.messages[..assistant_idx]
            .iter()
            .rposition(|m| {
                if let Message::User(u) = m {
                    u.content
                        .iter()
                        .any(|b| !matches!(b, ContentBlock::ToolResult { .. }))
                } else {
                    false
                }
            })?;

        let prompt = if let Message::User(u) = &s.messages[user_idx] {
            u.content.iter().find_map(|b| {
                if let ContentBlock::Text { text } = b {
                    Some(text.clone())
                } else {
                    None
                }
            })
        } else {
            None
        };

        // Drop the entire last turn (prompt + any tool results + assistants).
        s.messages.truncate(user_idx);

        if s.turn_count > 0 {
            s.turn_count -= 1;
        }

        prompt
    }

    /// Rewind the conversation by removing the last `n` turns (user+assistant pairs).
    ///
    /// Returns the number of turns actually removed and remaining message count.
    pub async fn rewind_turns(&self, n: usize) -> (usize, usize) {
        let mut s = self.state.write().await;
        let mut removed = 0;

        while removed < n && !s.messages.is_empty() {
            // Remove trailing assistant messages (and tool_result messages between them)
            let mut found_assistant = false;
            while let Some(last) = s.messages.last() {
                if matches!(last, Message::Assistant(_)) {
                    s.messages.pop();
                    found_assistant = true;
                    break;
                }
                // Remove tool_result / system messages trailing after the pair
                if found_assistant {
                    break;
                }
                s.messages.pop();
            }
            // Remove the preceding user message
            if found_assistant {
                if let Some(last) = s.messages.last() {
                    if matches!(last, Message::User(_)) {
                        s.messages.pop();
                    }
                }
                if s.turn_count > 0 {
                    s.turn_count -= 1;
                }
                removed += 1;
            } else {
                break; // no more assistant messages to remove
            }
        }

        (removed, s.messages.len())
    }
}
