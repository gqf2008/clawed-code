//! /init, /commit, /pr, /bug command handlers.

use crate::output::print_stream;
use clawed_agent::engine::QueryEngine;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedPrompt {
    pub summary: String,
    pub prompt: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CommitPushPrPlan {
    Message(String),
    SubmitPrompt(PreparedPrompt),
    CommitThenPr {
        commit: PreparedPrompt,
        baseline_status: String,
        user_message: String,
    },
}

/// Initialize CLAUDE.md for the current project.
pub(crate) async fn handle_init(engine: &QueryEngine, cwd: &std::path::Path) {
    submit_prepared(engine, prepare_init_prompt(cwd), "Init error").await;
}

pub(crate) fn prepare_init_prompt(cwd: &std::path::Path) -> PreparedPrompt {
    let claude_md_path = cwd.join("CLAUDE.md");
    let existing = if claude_md_path.exists() {
        std::fs::read_to_string(&claude_md_path).ok()
    } else {
        None
    };

    let mut context_parts: Vec<String> = Vec::new();

    for manifest in &[
        "package.json",
        "Cargo.toml",
        "pyproject.toml",
        "go.mod",
        "pom.xml",
        "build.gradle",
        "Makefile",
        "CMakeLists.txt",
    ] {
        let path = cwd.join(manifest);
        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                let truncated = content.lines().take(50).collect::<Vec<_>>().join("\n");
                context_parts.push(format!("--- {} ---\n{}", manifest, truncated));
            }
        }
    }

    for readme in &["README.md", "README.rst", "README.txt", "README"] {
        let path = cwd.join(readme);
        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                let truncated = content.lines().take(80).collect::<Vec<_>>().join("\n");
                context_parts.push(format!("--- {} ---\n{}", readme, truncated));
            }
            break;
        }
    }

    for ci in &[
        ".github/workflows",
        ".gitlab-ci.yml",
        "Jenkinsfile",
        ".circleci/config.yml",
    ] {
        let path = cwd.join(ci);
        if path.exists() {
            context_parts.push(format!("CI config found: {}", ci));
        }
    }

    let context = if context_parts.is_empty() {
        "No manifest or README files found.".to_string()
    } else {
        context_parts.join("\n\n")
    };

    let prompt = if let Some(existing_content) = existing {
        format!(
            "The project at {} already has a CLAUDE.md. Analyze the current content and the project \
             context below. Suggest specific improvements as diffs. Do NOT silently overwrite.\n\n\
             Existing CLAUDE.md:\n```\n{}\n```\n\nProject context:\n{}\n\n\
             Propose concrete changes to improve the CLAUDE.md.",
            cwd.display(),
            existing_content,
            context
        )
    } else {
        format!(
            "Create a CLAUDE.md file for the project at {}. Analyze the project context below \
             and generate a concise CLAUDE.md that includes ONLY:\n\
             - Build, test, and lint commands (especially non-obvious ones)\n\
             - Code style rules that differ from language defaults\n\
             - Repo conventions (branch naming, commit style, PR process)\n\
             - Required env vars or setup steps\n\
             - Non-obvious architectural decisions or gotchas\n\n\
             Do NOT include: file-by-file structure, standard language conventions, generic advice.\n\n\
             Project context:\n{}\n\n\
             Use the Write tool to create CLAUDE.md in the project root.",
            cwd.display(),
            context
        )
    };

    PreparedPrompt {
        summary: "\x1b[35m[Init]\x1b[0m Analyzing project…".to_string(),
        prompt,
    }
}

/// Stage changes and commit with an AI-generated message.
pub(crate) async fn handle_commit(engine: &QueryEngine, cwd: &std::path::Path, user_message: &str) {
    match prepare_commit_prompt(cwd, user_message) {
        Ok(prepared) => submit_prepared(engine, prepared, "提交错误").await,
        Err(message) => println!("{}", message),
    }
}

pub(crate) fn prepare_commit_prompt(
    cwd: &std::path::Path,
    user_message: &str,
) -> Result<PreparedPrompt, String> {
    let status = match git_cmd(cwd, &["status", "--porcelain"]) {
        Some(output) => output,
        None => {
            let err_check = std::process::Command::new("git")
                .args(["status"])
                .current_dir(cwd)
                .output();
            return match err_check {
                Err(error) => Err(format!("不是 git 仓库或找不到 git: {}", error)),
                Ok(_) => Err("没有需要提交的更改。".to_string()),
            };
        }
    };

    let diff = git_cmd(cwd, &["diff", "--staged"]).unwrap_or_default();
    let unstaged_diff = git_cmd(cwd, &["diff"]).unwrap_or_default();
    let log = git_cmd(cwd, &["log", "--oneline", "-10"]).unwrap_or_default();

    let combined_diff = if diff.is_empty() {
        &unstaged_diff
    } else {
        &diff
    };
    let has_staged = !diff.is_empty();
    let uses_conventional = detect_conventional_commits(&log);
    let style_hint = if uses_conventional {
        "- 使用 Conventional Commits 格式 (feat:, fix:, refactor:, docs:, test:, chore: 等)\n"
    } else {
        ""
    };

    let user_name = git_cmd(cwd, &["config", "user.name"]).unwrap_or_default();
    let user_email = git_cmd(cwd, &["config", "user.email"]).unwrap_or_default();
    let coauthor_trailer = if !user_name.is_empty() && !user_email.is_empty() {
        "- 在提交信息末尾添加: Co-authored-by: claude-code-rs <noreply@claude-code.rs>\n"
            .to_string()
    } else {
        String::new()
    };

    let prompt = format!(
        "提交当前 git 仓库中的更改。\n\n\
         规则:\n\
         - 分析更改并创建清晰的提交信息\n\
         - 跟随下面最近提交的风格\n\
         {style_hint}\
         - 专注于 \"为什么\" 而不是 \"什么\"\n\
         - 保持信息简洁 (1 行摘要，可选正文)\n\
         - {stage_instruction}\n\
         - 绝不使用 --amend, --no-verify, 或 --force\n\
         - 绝不提交密钥或凭证\n\
         - 使用 `git add` 暂存特定文件，然后 `git commit -m \"message\"`\n\
         {coauthor_trailer}\
         {user_note}\n\
         最近提交:\n```\n{log}\n```\n\n\
         git status:\n```\n{status}\n```\n\n\
         差异:\n```diff\n{diff}\n```",
        style_hint = style_hint,
        stage_instruction = if has_staged {
            "更改已暂存 — 直接提交"
        } else {
            "使用 `git add <file>` 暂存相关文件（除非所有更改相关，否则不要用 `git add -A`）"
        },
        coauthor_trailer = coauthor_trailer,
        user_note = if user_message.is_empty() {
            String::new()
        } else {
            format!("\n用户关于此提交的说明: {}\n", user_message)
        },
        log = log.trim(),
        status = status.trim(),
        diff = if combined_diff.len() > 8000 {
            format!(
                "{}…\n[已截断, 共 {} 字节]",
                &combined_diff[..8000],
                combined_diff.len()
            )
        } else {
            combined_diff.to_string()
        },
    );

    Ok(PreparedPrompt {
        summary: "\x1b[35m[Commit]\x1b[0m 分析更改…".to_string(),
        prompt,
    })
}

/// Detect if recent commits follow conventional commits format.
fn detect_conventional_commits(log: &str) -> bool {
    let conventional_prefixes = [
        "feat:",
        "fix:",
        "refactor:",
        "docs:",
        "test:",
        "chore:",
        "style:",
        "perf:",
        "ci:",
        "build:",
        "revert:",
        "feat(",
        "fix(",
        "refactor(",
        "docs(",
        "test(",
        "chore(",
    ];
    let lines: Vec<&str> = log.lines().collect();
    if lines.len() < 3 {
        return false;
    }
    let conventional_count = lines
        .iter()
        .filter(|line| {
            let msg = line
                .split_whitespace()
                .skip(1)
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase();
            conventional_prefixes
                .iter()
                .any(|prefix| msg.starts_with(prefix))
        })
        .count();
    conventional_count * 2 >= lines.len()
}

/// Create or review a pull request.
pub(crate) async fn handle_pr(engine: &QueryEngine, custom_prompt: &str, cwd: &std::path::Path) {
    match prepare_pr_prompt(cwd, custom_prompt) {
        Ok(prepared) => submit_prepared(engine, prepared, "PR 错误").await,
        Err(message) => println!("{}", message),
    }
}

pub(crate) fn prepare_pr_prompt(
    cwd: &std::path::Path,
    custom_prompt: &str,
) -> Result<PreparedPrompt, String> {
    let gh_available = std::process::Command::new("gh")
        .args(["--version"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false);

    let current_branch = git_cmd(cwd, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
    if current_branch.is_empty() || current_branch == "HEAD" {
        return Err("无法获取当前分支名。请确保在 git 仓库中。".to_string());
    }

    let default_branch = git_cmd(cwd, &["rev-parse", "--abbrev-ref", "origin/HEAD"])
        .map(|branch| {
            branch
                .strip_prefix("origin/")
                .unwrap_or(&branch)
                .to_string()
        })
        .unwrap_or_else(|| "main".to_string());

    if current_branch == default_branch {
        return Err(format!(
            "当前在默认分支 ({})。请先创建并切换到功能分支。",
            default_branch
        ));
    }

    let unpushed = git_cmd(
        cwd,
        &[
            "log",
            "--oneline",
            &format!("origin/{}..HEAD", current_branch),
        ],
    )
    .unwrap_or_default();

    let mut summary_lines = Vec::new();
    if !unpushed.is_empty() {
        summary_lines.push("\x1b[33m发现未推送的提交，先推送到远程...\x1b[0m".to_string());
        let push_result = std::process::Command::new("git")
            .args(["push", "-u", "origin", &current_branch])
            .current_dir(cwd)
            .output();
        match push_result {
            Ok(output) if output.status.success() => {
                summary_lines.push(format!(
                    "\x1b[32m✓ 已推送到 origin/{}\x1b[0m",
                    current_branch
                ));
            }
            Ok(output) => {
                let error = String::from_utf8_lossy(&output.stderr);
                return Err(format!("推送失败: {}", error.trim()));
            }
            Err(error) => {
                return Err(format!("推送失败: {}", error));
            }
        }
    }

    let diff =
        git_cmd(cwd, &["diff", &format!("origin/{}...HEAD", default_branch)]).unwrap_or_default();
    let log = git_cmd(
        cwd,
        &[
            "log",
            "--oneline",
            &format!("origin/{}..HEAD", default_branch),
        ],
    )
    .unwrap_or_default();

    if diff.is_empty() && log.is_empty() {
        return Err(format!(
            "没有相对 {} 的新提交。请先推送一些更改。",
            default_branch
        ));
    }

    let user_note = if custom_prompt.is_empty() {
        String::new()
    } else {
        format!("\n用户的说明: {}\n", custom_prompt)
    };

    let existing_pr = if gh_available {
        std::process::Command::new("gh")
            .args(["pr", "view", "--json", "number,title,url", "--jq", ".url"])
            .current_dir(cwd)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
            .filter(|output| !output.is_empty())
    } else {
        None
    };

    if let Some(ref pr_url) = existing_pr {
        summary_lines.push(format!("\x1b[36m已存在 PR: {}\x1b[0m", pr_url));
    }

    let action = if existing_pr.is_some() {
        "更新"
    } else {
        "创建"
    };

    let prompt =
        format!(
            "帮我为分支 `{branch}` → `{base}` {action}一个 Pull Request。\n\n\
         规则:\n\
         - 分析下面的提交和 diff\n\
         - 生成清晰的 PR 标题和描述\n\
         - PR 标题应简洁且有描述性\n\
         - PR 描述应包括: 变更摘要、动机、测试说明\n\
         - 使用 markdown 格式化描述\n\
         {gh_instruction}\
         {user_note}\n\
         提交记录:\n```\n{log}\n```\n\n\
         差异:\n```diff\n{diff}\n```",
            branch = current_branch,
            base = default_branch,
            action = action,
            gh_instruction =
                if gh_available {
                    format!(
                "- 使用 `gh pr {} --title \"<title>\" --body \"<body>\"` 命令创建/更新 PR\n\
                 - 不要使用 --web 参数\n",
                if existing_pr.is_some() { "edit" } else { "create" },
            )
                } else {
                    "- gh CLI 不可用，请只输出 PR 标题和描述\n".to_string()
                },
            user_note = user_note,
            log = log.trim(),
            diff = if diff.len() > 12000 {
                format!("{}…\n[已截断, 共 {} 字节]", &diff[..12000], diff.len())
            } else {
                diff
            },
        );

    summary_lines.push(format!(
        "\x1b[35m[PR]\x1b[0m {} → {} ({})…",
        current_branch, default_branch, action
    ));

    Ok(PreparedPrompt {
        summary: summary_lines.join("\n"),
        prompt,
    })
}

/// Combined commit → push → PR workflow.
pub(crate) async fn handle_commit_push_pr(
    engine: &QueryEngine,
    cwd: &std::path::Path,
    user_message: &str,
) {
    match prepare_commit_push_pr(cwd, user_message) {
        CommitPushPrPlan::Message(message) => println!("{}", message),
        CommitPushPrPlan::SubmitPrompt(prepared) => {
            submit_prepared(engine, prepared, "PR 错误").await;
        }
        CommitPushPrPlan::CommitThenPr {
            baseline_status, ..
        } => {
            eprintln!("\x1b[35m[Step 1/3]\x1b[0m 提交更改...");
            handle_commit(engine, cwd, user_message).await;

            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let new_status = git_cmd(cwd, &["status", "--porcelain"]).unwrap_or_default();
            if new_status == baseline_status {
                eprintln!("\x1b[33m提交似乎未完成，中止工作流。\x1b[0m");
                return;
            }

            eprintln!("\x1b[35m[Step 2/3]\x1b[0m 推送和创建 PR...");
            handle_pr(engine, user_message, cwd).await;
            eprintln!("\x1b[35m[Step 3/3]\x1b[0m 完成！");
        }
    }
}

pub(crate) fn prepare_commit_push_pr(
    cwd: &std::path::Path,
    user_message: &str,
) -> CommitPushPrPlan {
    match git_cmd(cwd, &["status", "--porcelain"]) {
        Some(status) if !status.is_empty() => match prepare_commit_prompt(cwd, user_message) {
            Ok(commit) => CommitPushPrPlan::CommitThenPr {
                commit,
                baseline_status: status,
                user_message: user_message.to_string(),
            },
            Err(message) => CommitPushPrPlan::Message(message),
        },
        _ => {
            let current_branch =
                git_cmd(cwd, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
            let unpushed = git_cmd(
                cwd,
                &[
                    "log",
                    "--oneline",
                    &format!("origin/{}..HEAD", current_branch),
                ],
            )
            .unwrap_or_default();
            if unpushed.is_empty() {
                return CommitPushPrPlan::Message(
                    "没有待提交的更改，也没有未推送的提交。".to_string(),
                );
            }

            match prepare_pr_prompt(cwd, user_message) {
                Ok(mut prepared) => {
                    prepared.summary = format!(
                        "\x1b[36m没有新更改需要提交，但有未推送的提交，继续推送和创建 PR...\x1b[0m\n{}",
                        prepared.summary
                    );
                    CommitPushPrPlan::SubmitPrompt(prepared)
                }
                Err(message) => CommitPushPrPlan::Message(message),
            }
        }
    }
}

/// Helper to run a git command and return trimmed stdout.
fn git_cmd(cwd: &std::path::Path, args: &[&str]) -> Option<String> {
    std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|output| !output.is_empty())
}

/// Debug a problem with AI assistance.
pub(crate) async fn handle_bug(engine: &QueryEngine, custom_prompt: &str, cwd: &std::path::Path) {
    submit_prepared(
        engine,
        prepare_bug_prompt(cwd, custom_prompt),
        "Debug error",
    )
    .await;
}

pub(crate) fn prepare_bug_prompt(cwd: &std::path::Path, custom_prompt: &str) -> PreparedPrompt {
    let mut context_parts: Vec<String> = Vec::new();

    if let Ok(output) = std::process::Command::new("git")
        .args(["log", "--oneline", "-5"])
        .current_dir(cwd)
        .output()
    {
        let log = String::from_utf8_lossy(&output.stdout).to_string();
        if !log.is_empty() {
            context_parts.push(format!("Recent commits:\n```\n{}\n```", log.trim()));
        }
    }

    if let Ok(output) = std::process::Command::new("git")
        .args(["diff", "HEAD~1"])
        .current_dir(cwd)
        .output()
    {
        let diff = String::from_utf8_lossy(&output.stdout).to_string();
        if !diff.is_empty() {
            let truncated = if diff.len() > 6000 {
                format!("{}…\n[truncated]", &diff[..6000])
            } else {
                diff
            };
            context_parts.push(format!("Recent changes:\n```diff\n{}\n```", truncated));
        }
    }

    let context = if context_parts.is_empty() {
        "No git context available.".to_string()
    } else {
        context_parts.join("\n\n")
    };

    let user_note = if custom_prompt.is_empty() {
        "Help me identify and fix bugs in the recent changes.".to_string()
    } else {
        custom_prompt.to_string()
    };

    let prompt = format!(
        "Debug the following problem:\n\n{user_note}\n\n\
         Instructions:\n\
         - Read the relevant source files to understand the code\n\
         - Identify the root cause of the problem\n\
         - Suggest a specific fix with code changes\n\
         - If the problem description is vague, ask clarifying questions\n\n\
         {context}",
        user_note = user_note,
        context = context,
    );

    PreparedPrompt {
        summary: "\x1b[35m[Debug]\x1b[0m Investigating…".to_string(),
        prompt,
    }
}

/// /summary — ask the model to summarize the conversation so far.
pub(crate) async fn handle_summary(engine: &QueryEngine) {
    submit_prepared(engine, prepare_summary_prompt(), "Summary error").await;
}

pub(crate) fn prepare_summary_prompt() -> PreparedPrompt {
    PreparedPrompt {
        summary: "\x1b[35m[Summary]\x1b[0m Generating conversation summary…".to_string(),
        prompt: "Please provide a concise summary of our conversation so far, \
            including the key topics discussed, decisions made, and any pending items or next steps."
            .to_string(),
    }
}

async fn submit_prepared(engine: &QueryEngine, prepared: PreparedPrompt, error_label: &str) {
    println!("{}", prepared.summary);
    let model = { engine.state().read().await.model.clone() };
    let stream = engine.submit(&prepared.prompt).await;
    if let Err(error) = print_stream(stream, &model, Some(engine.cost_tracker()), None).await {
        eprintln!("\x1b[31m{}: {}\x1b[0m", error_label, error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_conventional_commits_yes() {
        let log = "abc1234 feat: add login page\n\
                    def5678 fix: resolve crash on startup\n\
                    ghi9012 docs: update README\n\
                    jkl3456 chore: bump dependencies";
        assert!(detect_conventional_commits(log));
    }

    #[test]
    fn test_detect_conventional_commits_no() {
        let log = "abc1234 Add login page\n\
                    def5678 Fix crash on startup\n\
                    ghi9012 Update README\n\
                    jkl3456 Bump dependencies";
        assert!(!detect_conventional_commits(log));
    }

    #[test]
    fn test_detect_conventional_commits_mixed() {
        let log = "abc1234 feat: add login page\n\
                    def5678 fix: resolve crash\n\
                    ghi9012 Update README\n\
                    jkl3456 chore: bump deps";
        assert!(detect_conventional_commits(log));
    }

    #[test]
    fn test_detect_conventional_commits_too_few() {
        let log = "abc1234 feat: add login\n\
                    def5678 fix something";
        assert!(!detect_conventional_commits(log));
    }

    #[test]
    fn test_detect_conventional_commits_with_scope() {
        let log = "abc feat(auth): add login\n\
                    def fix(ui): button color\n\
                    ghi refactor(api): simplify\n\
                    jkl test(core): add unit tests";
        assert!(detect_conventional_commits(log));
    }

    #[test]
    fn test_git_cmd_nonexistent_dir() {
        let result = git_cmd(std::path::Path::new("/nonexistent/path"), &["status"]);
        assert!(result.is_none());
    }
}
