//! `/review` command handler — code review on git changes or PRs.

use crate::output::print_stream;
use clawed_agent::engine::QueryEngine;

use super::prompt::PreparedPrompt;

/// Launch a code review on recent git changes or a specific PR.
pub(crate) async fn handle_review(
    engine: &QueryEngine,
    custom_prompt: &str,
    cwd: &std::path::Path,
) {
    match prepare_review_submission(custom_prompt, cwd) {
        Ok(prepared) => {
            eprintln!("{}", prepared.summary);
            let model = { engine.state().read().await.model.clone() };
            let stream = engine.submit(&prepared.prompt).await;
            if let Err(error) =
                print_stream(stream, &model, Some(engine.cost_tracker()), None).await
            {
                eprintln!("\x1b[31mReview 错误: {}\x1b[0m", error);
            }
        }
        Err(message) => println!("{}", message),
    }
}

pub(crate) fn prepare_review_submission(
    custom_prompt: &str,
    cwd: &std::path::Path,
) -> Result<PreparedPrompt, String> {
    let pr_number = custom_prompt
        .trim()
        .strip_prefix('#')
        .or(Some(custom_prompt.trim()))
        .and_then(|value| value.parse::<u64>().ok());

    if let Some(pr_num) = pr_number {
        return prepare_pr_review_submission(pr_num, cwd);
    }

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
                    return Err("没有可 review 的更改。先做一些更改吧。".to_string());
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

    let file_stats = parse_diff_file_stats(&diff);
    let summary = if !file_stats.is_empty() {
        let total_added: usize = file_stats.iter().map(|(_, added, _)| added).sum();
        let total_removed: usize = file_stats.iter().map(|(_, _, removed)| removed).sum();
        format!(
            "\x1b[35m[Code Review]\x1b[0m {} 更改 ({}) — \x1b[32m+{}\x1b[0m / \x1b[31m-{}\x1b[0m 行\n{}",
            file_stats.len(),
            source,
            total_added,
            total_removed,
            format_file_stats(&file_stats)
        )
    } else {
        "\x1b[35m[Code Review]\x1b[0m".to_string()
    };

    let prompt = if custom_prompt.is_empty() {
        format!(
            "Review 以下代码更改，检查 bug、风格问题、安全隐患和改进建议。\
             请具体指出文件路径和行号。\n\n\
             ```diff\n{}\n```",
            diff
        )
    } else {
        format!("{}\n\n```diff\n{}\n```", custom_prompt, diff)
    };

    Ok(PreparedPrompt { summary, prompt })
}

fn format_file_stats(file_stats: &[(String, usize, usize)]) -> String {
    let mut out = String::new();
    for (file, added, removed) in file_stats {
        let stat = if *added > 0 && *removed > 0 {
            format!("\x1b[32m+{}\x1b[0m/\x1b[31m-{}\x1b[0m", added, removed)
        } else if *added > 0 {
            format!("\x1b[32m+{}\x1b[0m", added)
        } else {
            format!("\x1b[31m-{}\x1b[0m", removed)
        };
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!("  \x1b[2m•\x1b[0m {} ({})", file, stat));
    }
    out
}

/// Parse a unified diff to extract per-file line statistics.
/// Returns Vec of (filename, lines_added, lines_removed).
pub(crate) fn parse_diff_file_stats(diff: &str) -> Vec<(String, usize, usize)> {
    let mut results: Vec<(String, usize, usize)> = Vec::new();
    let mut current_file: Option<String> = None;
    let mut added = 0usize;
    let mut removed = 0usize;

    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("+++ b/") {
            if let Some(file) = current_file.take() {
                if added > 0 || removed > 0 {
                    results.push((file, added, removed));
                }
            }
            current_file = Some(rest.to_string());
            added = 0;
            removed = 0;
        } else if line.starts_with('+') && !line.starts_with("+++") {
            added += 1;
        } else if line.starts_with('-') && !line.starts_with("---") {
            removed += 1;
        }
    }

    if let Some(file) = current_file {
        if added > 0 || removed > 0 {
            results.push((file, added, removed));
        }
    }
    results
}

fn prepare_pr_review_submission(
    pr_number: u64,
    cwd: &std::path::Path,
) -> Result<PreparedPrompt, String> {
    let gh_available = std::process::Command::new("gh")
        .args(["--version"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false);

    if !gh_available {
        return Err("需要 gh CLI 来 review PR。请安装: https://cli.github.com".to_string());
    }

    let pr_info = std::process::Command::new("gh")
        .args([
            "pr",
            "view",
            &pr_number.to_string(),
            "--json",
            "title,body,author,state,additions,deletions",
        ])
        .current_dir(cwd)
        .output();

    let pr_meta = match pr_info {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).to_string()
        }
        Ok(output) => {
            let error = String::from_utf8_lossy(&output.stderr);
            return Err(format!("获取 PR #{} 信息失败: {}", pr_number, error.trim()));
        }
        Err(error) => return Err(format!("运行 gh 失败: {}", error)),
    };

    let pr_diff = std::process::Command::new("gh")
        .args(["pr", "diff", &pr_number.to_string()])
        .current_dir(cwd)
        .output()
        .ok()
        .map(|output| String::from_utf8_lossy(&output.stdout).to_string())
        .unwrap_or_default();

    if pr_diff.is_empty() {
        return Err(format!("PR #{} 没有差异内容。", pr_number));
    }

    let file_stats = parse_diff_file_stats(&pr_diff);
    let summary = if !file_stats.is_empty() {
        let total_added: usize = file_stats.iter().map(|(_, added, _)| added).sum();
        let total_removed: usize = file_stats.iter().map(|(_, _, removed)| removed).sum();
        format!(
            "\x1b[35m[PR Review]\x1b[0m PR #{} — {} 文件, \x1b[32m+{}\x1b[0m / \x1b[31m-{}\x1b[0m 行\n{}",
            pr_number,
            file_stats.len(),
            total_added,
            total_removed,
            format_file_stats(&file_stats)
        )
    } else {
        format!("\x1b[35m[PR Review]\x1b[0m 正在分析 PR #{}…", pr_number)
    };

    let truncated_diff = if pr_diff.len() > 15000 {
        format!(
            "{}…\n[已截断, 共 {} 字节]",
            &pr_diff[..15000],
            pr_diff.len()
        )
    } else {
        pr_diff
    };

    let prompt = format!(
        "Review PR #{num} 的代码更改。\n\n\
         PR 信息:\n```json\n{meta}\n```\n\n\
         请检查:\n\
         - Bug 和逻辑错误\n\
         - 安全隐患\n\
         - 代码风格和最佳实践\n\
         - 性能问题\n\
         - 测试覆盖\n\n\
         请具体指出文件路径和行号，给出可操作的改进建议。\n\n\
         差异:\n```diff\n{diff}\n```",
        num = pr_number,
        meta = pr_meta.trim(),
        diff = truncated_diff,
    );

    Ok(PreparedPrompt { summary, prompt })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_diff_file_stats_basic() {
        let diff = "\
diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,3 +1,5 @@
 fn main() {
-    println!(\"hello\");
+    println!(\"hello world\");
+    println!(\"goodbye\");
+    // new comment
 }
";
        let stats = parse_diff_file_stats(diff);
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].0, "src/main.rs");
        assert_eq!(stats[0].1, 3);
        assert_eq!(stats[0].2, 1);
    }

    #[test]
    fn test_parse_diff_file_stats_multi_file() {
        let diff = "\
diff --git a/a.rs b/a.rs
--- a/a.rs
+++ b/a.rs
@@ -1 +1,2 @@
 line1
+line2
diff --git a/b.rs b/b.rs
--- a/b.rs
+++ b/b.rs
@@ -1,2 +1 @@
 keep
-remove
";
        let stats = parse_diff_file_stats(diff);
        assert_eq!(stats.len(), 2);
        assert_eq!(stats[0], ("a.rs".to_string(), 1, 0));
        assert_eq!(stats[1], ("b.rs".to_string(), 0, 1));
    }

    #[test]
    fn test_parse_diff_file_stats_empty() {
        assert!(parse_diff_file_stats("").is_empty());
        assert!(parse_diff_file_stats("no diff here").is_empty());
    }
}
