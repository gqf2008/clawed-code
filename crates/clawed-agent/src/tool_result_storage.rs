//! Tool result disk persistence.
//!
//! When a tool result exceeds a size threshold, the full content is written to
//! disk and the model receives a short preview plus a file path it can `Read`
//! to access the full output. This prevents huge tool outputs from consuming
//! the context window.
//!
//! Mirrors TS `toolResultStorage.ts`.

use std::path::Path;

/// Maximum tool result size (in chars) before persisting to disk.
const MAX_RESULT_CHARS: usize = 50_000;

/// Preview size in bytes for the inline reference message.
const PREVIEW_SIZE_BYTES: usize = 2000;

/// Directory name under the session dir for persisted tool results.
const TOOL_RESULTS_SUBDIR: &str = "tool-results";

/// Tag wrapping a persisted tool result reference.
const PERSISTED_OUTPUT_TAG: &str = "<system-reminder>\nDEPRECATED: This tool call output has been stored to a file. Use the Read tool to read the full output from the file path provided below.\n</system-reminder>\n\n<persisted-output>";

/// Closing tag.
const PERSISTED_OUTPUT_CLOSING_TAG: &str = "</persisted-output>";

/// Check if a tool result text exceeds the threshold and should be persisted.
pub fn should_persist(text: &str) -> bool {
    text.len() > MAX_RESULT_CHARS
}

/// Persist a large tool result to disk and return a preview reference message.
///
/// The full content is written to `<session_dir>/tool-results/<tool_use_id>.txt`.
/// Returns `Ok(reference_message)` on success, `Err(text)` if persistence fails
/// (caller should use the original text).
pub fn persist_tool_result(
    text: &str,
    tool_use_id: &str,
    session_dir: &Path,
) -> Result<String, String> {
    let dir = session_dir.join(TOOL_RESULTS_SUBDIR);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return Err(format!("Failed to create tool results dir: {e}"));
    }

    let filepath = dir.join(format!("{tool_use_id}.txt"));

    // Don't overwrite if already persisted (e.g., during compaction re-run)
    if !filepath.exists() {
        if let Err(e) = std::fs::write(&filepath, text) {
            return Err(format!("Failed to write tool result: {e}"));
        }
    }

    let (preview, has_more) = generate_preview(text, PREVIEW_SIZE_BYTES);
    let original_size = text.len();

    let path_str = filepath.to_string_lossy().replace('\\', "/");
    let size_str = format_size(original_size);
    let preview_size_str = format_size(PREVIEW_SIZE_BYTES);

    let mut msg = format!(
        "{PERSISTED_OUTPUT_TAG}\n\
         Output too large ({size_str}). Full output saved to: {path_str}\n\n\
         Preview (first {preview_size_str}):\n\
         {preview}"
    );
    if has_more {
        msg.push_str("\n...\n");
    }
    msg.push('\n');
    msg.push_str(PERSISTED_OUTPUT_CLOSING_TAG);

    Ok(msg)
}

/// Generate a preview from the text, truncated at a byte boundary.
fn generate_preview(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_string(), false);
    }

    // Find a safe byte boundary
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_string(), true)
}

/// Format a byte count as a human-readable size.
fn format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes} bytes")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_persist() {
        assert!(!should_persist(&"x".repeat(49_999)));
        assert!(!should_persist(&"x".repeat(50_000)));
        assert!(should_persist(&"x".repeat(50_001)));
    }

    #[test]
    fn test_generate_preview_short() {
        let (preview, has_more) = generate_preview("hello", 100);
        assert_eq!(preview, "hello");
        assert!(!has_more);
    }

    #[test]
    fn test_generate_preview_long() {
        let text = "x".repeat(5000);
        let (preview, has_more) = generate_preview(&text, 100);
        assert!(preview.len() <= 100);
        assert!(has_more);
    }

    #[test]
    fn test_generate_preview_utf8_boundary() {
        // 3-byte UTF-8 characters
        let text = "你好世界".repeat(1000);
        let (preview, has_more) = generate_preview(&text, 100);
        assert!(preview.len() <= 103); // Allow for boundary adjustment
        assert!(has_more);
        // Verify valid UTF-8
        assert!(std::str::from_utf8(preview.as_bytes()).is_ok());
    }

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(500), "500 bytes");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(2_000_000), "1.9 MB");
    }

    #[test]
    fn test_persist_tool_result_writes_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let text = "x".repeat(60_000);
        let result = persist_tool_result(&text, "test-id-123", tmp.path());
        assert!(result.is_ok());

        let msg = result.unwrap();
        assert!(msg.contains("<persisted-output>"));
        assert!(msg.contains("</persisted-output>"));
        assert!(msg.contains("test-id-123.txt"));
        assert!(msg.contains("Preview"));

        // Verify file exists
        let file_path = tmp.path().join("tool-results").join("test-id-123.txt");
        assert!(file_path.exists());
        assert_eq!(std::fs::read_to_string(&file_path).unwrap().len(), 60_000);
    }

    #[test]
    fn test_persist_tool_result_small_returns_err() {
        // This shouldn't normally be called for small results, but verify it works
        let tmp = tempfile::TempDir::new().unwrap();
        let text = "hello";
        let result = persist_tool_result(text, "small-id", tmp.path());
        // It still works — caller checks should_persist() first
        assert!(result.is_ok());
    }
}
