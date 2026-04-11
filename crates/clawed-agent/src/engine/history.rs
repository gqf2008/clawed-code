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
    /// Removes both the last assistant message and the last user message from history.
    /// Used by `/retry` to resend the last user prompt.
    pub async fn pop_last_turn(&self) -> Option<String> {
        let mut s = self.state.write().await;

        // Extract the last user prompt while holding the write lock
        let prompt = s.messages.iter().rev().find_map(|m| {
            if let Message::User(u) = m {
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
        })?;

        // Pop messages from the end until we've removed the last assistant + user pair
        let mut removed_assistant = false;
        while let Some(last) = s.messages.last() {
            match last {
                Message::Assistant(_) if !removed_assistant => {
                    s.messages.pop();
                    removed_assistant = true;
                }
                Message::User(_) if removed_assistant => {
                    s.messages.pop();
                    break;
                }
                _ if removed_assistant => {
                    break; // stop if we hit a non-user message
                }
                _ => {
                    s.messages.pop(); // skip tool result messages etc.
                }
            }
        }
        if s.turn_count > 0 {
            s.turn_count -= 1;
        }

        Some(prompt)
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
