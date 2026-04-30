//! `/simplify` command handler — simplify and refactor code.
//!
//! Based on the official Claude Code "Simplify" system prompt:
//! - Phase 1: Identify changes via git diff
//! - Phase 2: Launch three review agents in parallel (reuse, quality, efficiency)
//! - Phase 3: Fix issues found

use crate::output::print_stream;
use clawed_agent::engine::QueryEngine;
use clawed_tools::path_util::{is_binary_extension, resolve_path_safe};

use super::prompt::PreparedPrompt;

/// Maximum file size to read (10 MB).
const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024;

const SIMPLIFY_SYSTEM_PROMPT: &str = "Review all changed files for reuse, quality, and efficiency. Fix any issues found.\n\n\
## Phase 1: Identify Changes\n\n\
## Phase 2: Review\n\n\
Perform three parallel reviews on the changes above:\n\n\
### 1. Code Reuse Review\n\
- Search for existing utilities and helpers that could replace newly written code\n\
- Flag any new function that duplicates existing functionality\n\
- Flag any inline logic that could use an existing utility\n\n\
### 2. Code Quality Review\n\
Check for:\n\
- Redundant state that duplicates existing state\n\
- Parameter sprawl (adding new parameters instead of generalizing)\n\
- Copy-paste with slight variation\n\
- Leaky abstractions\n\
- Stringly-typed code where constants/enums exist\n\
- Nested conditionals (ternary chains, nested if/else 3+ levels)\n\
- Unnecessary comments explaining WHAT instead of WHY\n\n\
### 3. Efficiency Review\n\
Check for:\n\
- Unnecessary work (redundant computations, repeated reads, N+1 patterns)\n\
- Missed concurrency (independent operations run sequentially)\n\
- Hot-path bloat\n\
- Unnecessary existence checks (TOCTOU anti-pattern)\n\
- Memory issues (unbounded data structures, missing cleanup)\n\
- Overly broad operations (reading entire files when only portion needed)\n\n\
## Phase 3: Fix Issues\n\n\
For each real issue found, apply the fix directly. If a finding is a false positive, skip it.\n\
When done, briefly summarize what was fixed (or confirm the code was already clean).";

/// Launch a code simplification session on recent git changes or specific files.
pub(crate) async fn handle_simplify(
    engine: &QueryEngine,
    custom_prompt: &str,
    cwd: &std::path::Path,
) {
    match prepare_simplify_submission(custom_prompt, cwd) {
        Ok(prepared) => {
            eprintln!("{}", prepared.summary);
            let model = { engine.state().read().await.model.clone() };
            let stream = engine.submit(&prepared.prompt).await;
            if let Err(error) =
                print_stream(stream, &model, Some(engine.cost_tracker()), None).await
            {
                eprintln!("\x1b[31mSimplify 错误: {}\x1b[0m", error);
            }
        }
        Err(message) => println!("{}", message),
    }
}

pub(crate) fn prepare_simplify_submission(
    custom_prompt: &str,
    cwd: &std::path::Path,
) -> Result<PreparedPrompt, String> {
    let args = custom_prompt.trim();

    // If the first argument is an existing file path, treat all as file paths
    let first_token = args.split_whitespace().next();
    let has_file_path = first_token.is_some_and(|t| {
        resolve_path_safe(t, cwd).is_ok_and(|p| p.is_file())
    });

    if has_file_path {
        return prepare_file_simplify(args, cwd);
    }

    // Fall back to git diff based approach
    let diff_output = std::process::Command::new("git")
        .args(["diff", "HEAD"])
        .current_dir(cwd)
        .output();

    let (diff, source) = match diff_output {
        Ok(output) => {
            let diff = String::from_utf8_lossy(&output.stdout).to_string();
            if diff.is_empty() {
                let staged = std::process::Command::new("git")
                    .args(["diff", "--cached"])
                    .current_dir(cwd)
                    .output()
                    .ok()
                    .map(|output| String::from_utf8_lossy(&output.stdout).to_string())
                    .unwrap_or_default();
                if staged.is_empty() {
                    return Err("没有可简化的更改。先做一些更改，或指定文件路径。".to_string());
                }
                (staged, "staged")
            } else {
                (diff, "unstaged")
            }
        }
        Err(error) => {
            return Err(format!("获取 git diff 失败: {}", error));
        }
    };

    let file_stats = crate::repl_commands::review::parse_diff_file_stats(&diff);
    let summary = if !file_stats.is_empty() {
        let total_added: usize = file_stats.iter().map(|(_, added, _)| added).sum();
        let total_removed: usize = file_stats.iter().map(|(_, _, removed)| removed).sum();
        format!(
            "\x1b[35m[Simplify]\x1b[0m {} 更改 ({}) — \x1b[32m+{}\x1b[0m / \x1b[31m-{}\x1b[0m 行",
            file_stats.len(),
            source,
            total_added,
            total_removed,
        )
    } else {
        "\x1b[35m[Simplify]\x1b[0m".to_string()
    };

    let prompt = if args.is_empty() {
        format!(
            "{}\n\n\
             The following diff shows the changes to review:\n\n\
             ```diff\n{}\n```",
            SIMPLIFY_SYSTEM_PROMPT, diff
        )
    } else {
        format!(
            "{}\n\n\
             Additional instructions from user: {}\n\n\
             The following diff shows the changes to review:\n\n\
             ```diff\n{}\n```",
            SIMPLIFY_SYSTEM_PROMPT, custom_prompt, diff
        )
    };

    Ok(PreparedPrompt { summary, prompt })
}

fn prepare_file_simplify(args: &str, cwd: &std::path::Path) -> Result<PreparedPrompt, String> {
    let paths: Vec<&str> = args.split_whitespace().collect();
    let mut file_contents = String::new();
    let mut found_files = 0;

    for path_str in &paths {
        // Resolve and validate path (prevents path traversal)
        let path = match resolve_path_safe(path_str, cwd) {
            Ok(p) => p,
            Err(e) => return Err(format!("路径 '{}' 无效: {}", path_str, e)),
        };

        if !path.is_file() {
            return Err(format!("'{}' 不是文件或不存在。", path_str));
        }

        // Reject binary files
        if is_binary_extension(&path) {
            return Err(format!("'{}' 是二进制文件，跳过。", path_str));
        }

        // Check file size before reading
        let metadata = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(e) => return Err(format!("无法获取 '{}' 的元数据: {}", path_str, e)),
        };
        if metadata.len() > MAX_FILE_SIZE {
            return Err(format!(
                "'{}' 文件过大 ({} > {} MB)。",
                path_str,
                metadata.len() / 1024 / 1024,
                MAX_FILE_SIZE / 1024 / 1024,
            ));
        }

        match std::fs::read_to_string(&path) {
            Ok(content) => {
                found_files += 1;
                if !file_contents.is_empty() {
                    file_contents.push_str("\n\n");
                }
                file_contents.push_str(&format!("--- {} ---\n", path_str));
                file_contents.push_str(&content);
            }
            Err(e) => {
                return Err(format!("无法读取文件 '{}': {}", path_str, e));
            }
        }
    }

    if found_files == 0 {
        return Err("未找到可简化的文件。".to_string());
    }

    let summary = format!("\x1b[35m[Simplify]\x1b[0m {} 文件", found_files);

    let prompt = format!(
        "{}\n\n\
         The following files need review:\n\n\
         ```\n{}\n```",
        SIMPLIFY_SYSTEM_PROMPT, file_contents
    );

    Ok(PreparedPrompt { summary, prompt })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_prepare_file_simplify_reads_file() {
        let tmp = std::env::temp_dir().join("clawed_test_simplify");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let file = tmp.join("test.rs");
        std::fs::write(&file, "fn main() {}").unwrap();

        let result = prepare_file_simplify("test.rs", &tmp);
        assert!(result.is_ok());
        let prepared = result.unwrap();
        assert!(prepared.summary.contains("1 文件"));
        assert!(prepared.prompt.contains("fn main()"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_prepare_file_simplify_rejects_nonexistent() {
        let tmp = std::env::temp_dir().join("clawed_test_simplify2");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let result = prepare_file_simplify("nonexistent.rs", &tmp);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("不存在"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_prepare_file_simplify_rejects_binary() {
        let tmp = std::env::temp_dir().join("clawed_test_simplify3");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let file = tmp.join("image.png");
        std::fs::write(&file, b"\x89PNG\r\n\x1a\n").unwrap();

        let result = prepare_file_simplify("image.png", &tmp);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("二进制"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_prepare_file_simplify_rejects_oversized() {
        let tmp = std::env::temp_dir().join("clawed_test_simplify4");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let file = tmp.join("big.rs");
        let mut f = std::fs::File::create(&file).unwrap();
        let data = vec![b'x'; (MAX_FILE_SIZE + 1) as usize];
        f.write_all(&data).unwrap();
        drop(f);

        let result = prepare_file_simplify("big.rs", &tmp);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("过大"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_prepare_file_simplify_rejects_path_traversal() {
        let tmp = std::env::temp_dir().join("clawed_test_simplify5");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let result = prepare_file_simplify("../etc/passwd", &tmp);
        assert!(result.is_err());

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
