//! Filesystem path safety — protects critical directories and config files from
//! accidental editing/writing.  Aligned with TS `utils/permissions/filesystem.ts`.

use std::path::Path;

/// Directories whose contents are protected from auto-edit.
/// Case-insensitive match on any path segment.
const DANGEROUS_DIRECTORIES: &[&str] = &[".git", ".vscode", ".idea", ".claude"];

/// Filenames that are always protected from auto-edit.
/// Case-insensitive match on the final path segment.
const DANGEROUS_FILES: &[&str] = &[
    ".gitconfig",
    ".gitmodules",
    ".bashrc",
    ".bash_profile",
    ".zshrc",
    ".zprofile",
    ".profile",
    ".ripgreprc",
    ".mcp.json",
    ".claude.json",
];

/// Check if a file path is dangerous to auto-edit.
///
/// Returns `Some(reason)` if the path should be blocked, `None` if it's safe.
/// Checks the path against protected directories, files, and Claude config paths.
pub fn check_path_safety(path: &Path) -> Option<String> {
    let path_str = path.to_string_lossy();
    let path_lower = path_str.to_lowercase();

    // Check for shell expansion patterns in path
    if path_lower.contains('$')
        || path_lower.contains('%')
        || path_lower.contains("~")
        || path_lower.contains('\\')
            && (path_lower.starts_with("//") || path_lower.starts_with("\\\\"))
    {
        // Allow if it's just a Windows drive letter prefix
        if !(path_lower.len() > 2
            && path_lower.as_bytes()[1] == b':'
            && path_lower.as_bytes()[0].is_ascii_alphabetic())
        {
            return Some("Path contains shell expansion or UNC patterns".into());
        }
    }

    // Check for glob patterns (write tools shouldn't expand globs)
    let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if filename.contains('*') || filename.contains('?') {
        return Some("Path contains glob patterns — write tools don't expand globs".into());
    }

    // Check each path segment against dangerous directories
    for component in path.components() {
        if let std::path::Component::Normal(os_str) = component {
            if let Some(segment) = os_str.to_str() {
                let seg_lower = segment.to_lowercase();
                if DANGEROUS_DIRECTORIES.contains(&seg_lower.as_str()) {
                    // Exception: .claude/worktrees/ is allowed
                    if seg_lower == ".claude" && is_claude_worktrees_path(path) {
                        continue;
                    }
                    return Some(format!(
                        "Protected directory: {}/ — editing contents requires explicit approval",
                        segment
                    ));
                }
            }
        }
    }

    // Check filename against dangerous files
    if !filename.is_empty() {
        let fname_lower = filename.to_lowercase();
        if DANGEROUS_FILES.contains(&fname_lower.as_str()) {
            return Some(format!(
                "Protected file: {} — editing requires explicit approval",
                filename
            ));
        }
    }

    // Check for Claude config files (settings, commands, agents, skills)
    if let Some(reason) = check_claude_config_path(path) {
        return Some(reason);
    }

    None
}

/// Check if a path is under .claude/worktrees/ (allowed exception).
fn is_claude_worktrees_path(path: &Path) -> bool {
    let components: Vec<_> = path
        .components()
        .filter_map(|c| {
            if let std::path::Component::Normal(os_str) = c {
                os_str.to_str().map(|s| s.to_lowercase())
            } else {
                None
            }
        })
        .collect();

    for window in components.windows(2) {
        if window[0] == ".claude" && window[1] == "worktrees" {
            return true;
        }
    }
    false
}

/// Check for Claude config files that require explicit approval to edit.
fn check_claude_config_path(path: &Path) -> Option<String> {
    let components: Vec<_> = path
        .components()
        .filter_map(|c| {
            if let std::path::Component::Normal(os_str) = c {
                os_str.to_str().map(|s| s.to_lowercase())
            } else {
                None
            }
        })
        .collect();

    // Look for .claude directory in the path
    for (i, component) in components.iter().enumerate() {
        if component == ".claude" {
            let after = &components[i + 1..];

            // .claude/settings.json or .claude/settings.local.json
            if let Some(next) = after.first() {
                if *next == "settings.json" || *next == "settings.local.json" {
                    return Some(
                        "Protected: .claude/settings — editing requires explicit approval".into(),
                    );
                }
                // .claude/commands/, .claude/agents/, .claude/skills/
                if *next == "commands" || *next == "agents" || *next == "skills" {
                    return Some(format!(
                        "Protected: .claude/{} — editing requires explicit approval",
                        next
                    ));
                }
            }
        }
    }

    None
}

/// Check if a path looks like a dangerous removal target (rm -rf).
/// Returns Some(reason) if the path should be blocked from bulk removal.
pub fn check_dangerous_removal_path(path: &Path) -> Option<String> {
    let path_str = path.to_string_lossy();

    // Root paths
    if path_str == "/" || path_str == "~" || path_str.is_empty() {
        return Some("Cannot remove root or home directory".into());
    }

    // Windows drive roots
    let lower = path_str.to_lowercase();
    if lower.len() == 3 && lower.chars().nth(1) == Some(':') && lower.ends_with('\\') {
        return Some(format!("Cannot remove drive root: {}", path_str));
    }
    if lower.len() == 2 && lower.chars().nth(1) == Some(':') {
        return Some(format!("Cannot remove drive root: {}", path_str));
    }

    // Direct children of common system directories
    let dangerous_parents = [
        "/usr", "/etc", "/var", "/sys", "/proc", "/boot", "/dev", "/tmp", "/opt",
    ];
    for parent in &dangerous_parents {
        if lower.starts_with(parent) && lower.trim_start_matches(parent).matches('/').count() <= 1 {
            return Some(format!(
                "Cannot remove system directory contents: {}",
                path_str
            ));
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // ── check_path_safety: dangerous directories ─────────────────────────

    #[test]
    fn blocks_git_directory() {
        let path = PathBuf::from("/project/.git/config");
        assert!(check_path_safety(&path).is_some());
    }

    #[test]
    fn blocks_vscode_directory() {
        let path = PathBuf::from("/project/.vscode/settings.json");
        assert!(check_path_safety(&path).is_some());
    }

    #[test]
    fn blocks_idea_directory() {
        let path = PathBuf::from("/project/.idea/workspace.xml");
        assert!(check_path_safety(&path).is_some());
    }

    #[test]
    fn blocks_claude_directory() {
        let path = PathBuf::from("/project/.claude/some_file");
        assert!(check_path_safety(&path).is_some());
    }

    #[test]
    fn allows_claude_worktrees() {
        let path = PathBuf::from("/project/.claude/worktrees/abc123/src/main.rs");
        assert!(check_path_safety(&path).is_none());
    }

    #[test]
    fn allows_normal_path() {
        let path = PathBuf::from("/project/src/main.rs");
        assert!(check_path_safety(&path).is_none());
    }

    // ── check_path_safety: dangerous files ───────────────────────────────

    #[test]
    fn blocks_bashrc() {
        let path = PathBuf::from("/home/user/.bashrc");
        assert!(check_path_safety(&path).is_some());
    }

    #[test]
    fn blocks_gitconfig() {
        let path = PathBuf::from("/home/user/.gitconfig");
        assert!(check_path_safety(&path).is_some());
    }

    #[test]
    fn blocks_mcp_json() {
        let path = PathBuf::from("/project/.mcp.json");
        assert!(check_path_safety(&path).is_some());
    }

    #[test]
    fn blocks_claude_json() {
        let path = PathBuf::from("/project/.claude.json");
        assert!(check_path_safety(&path).is_some());
    }

    #[test]
    fn case_insensitive_dangerous_file() {
        let path = PathBuf::from("/home/user/.BASHRC");
        assert!(check_path_safety(&path).is_some());
    }

    // ── check_path_safety: Claude config ─────────────────────────────────

    #[test]
    fn blocks_claude_settings() {
        let path = PathBuf::from("/project/.claude/settings.json");
        assert!(check_path_safety(&path).is_some());
    }

    #[test]
    fn blocks_claude_settings_local() {
        let path = PathBuf::from("/project/.claude/settings.local.json");
        assert!(check_path_safety(&path).is_some());
    }

    #[test]
    fn blocks_claude_commands() {
        let path = PathBuf::from("/project/.claude/commands/review.md");
        assert!(check_path_safety(&path).is_some());
    }

    #[test]
    fn blocks_claude_skills() {
        let path = PathBuf::from("/project/.claude/skills/test.md");
        assert!(check_path_safety(&path).is_some());
    }

    // ── check_path_safety: glob patterns ─────────────────────────────────

    #[test]
    fn blocks_glob_in_filename() {
        let path = PathBuf::from("/project/src/*.rs");
        assert!(check_path_safety(&path).is_some());
    }

    // ── check_dangerous_removal_path ─────────────────────────────────────

    #[test]
    fn blocks_root_removal() {
        assert!(check_dangerous_removal_path(Path::new("/")).is_some());
    }

    #[test]
    fn blocks_home_removal() {
        assert!(check_dangerous_removal_path(Path::new("~")).is_some());
    }

    #[test]
    fn allows_subdirectory_removal() {
        assert!(check_dangerous_removal_path(Path::new("/project/build")).is_none());
    }

    #[test]
    fn blocks_system_dir_removal() {
        assert!(check_dangerous_removal_path(Path::new("/usr")).is_some());
    }
}
