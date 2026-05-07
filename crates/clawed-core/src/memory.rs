//! Memory system — mirrors claude-code's `~/.claude/projects/<key>/memory/` file-based memory.
//!
//! # Design (aligned with original TypeScript)
//!
//! Memory files are plain `.md` files living under project-isolated directories:
//!   - `~/.claude/projects/<sanitized-git-root>/memory/`  (project-isolated)
//!   - `<project>/.claude/memory/`                        (project-scoped, in-repo)
//!   - `~/.claude/memory/`                                (legacy user-global, backward compat)
//!
//! The project key is derived from the canonical git root path, sanitized via
//! `git_util::sanitize_path_key()` (replaces non-alphanumeric → `-`).
//!
//! Each file **may** start with a YAML frontmatter block (between `---` markers)
//! containing:
//!   - `type:` one of `user | feedback | project | reference | team | agent`
//!   - `description:` short one-liner shown in the manifest
//!
//! ## Injection strategy
//!
//! `load_memories_for_prompt()` returns a formatted block that is prepended to
//! the system prompt (same approach as CLAUDE.md injection).  For context
//! efficiency we include:
//!   1. A compact manifest (one line per file) so Claude knows what's available.
//!   2. The full content of each file (up to `MAX_MEMORY_BYTES` per file,
//!      `MAX_TOTAL_BYTES` total).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use tracing::warn;

// ── Constants ────────────────────────────────────────────────────────────────

const MAX_MEMORY_FILES: usize = 200;
const MAX_MEMORY_BYTES_PER_FILE: usize = 10_000;
const MAX_TOTAL_MEMORY_BYTES: usize = 100_000;

// ── MEMORY.md index constraints (TS parity: memdir.ts) ──────────────────────
const MAX_ENTRYPOINT_LINES: usize = 200;
const MAX_ENTRYPOINT_BYTES: usize = 25_000;

/// Name of the index file (always loaded, never counted as a memory file itself).
pub const MEMORY_INDEX_FILENAME: &str = "MEMORY.md";

// ── Memory types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryType {
    User,
    Feedback,
    Project,
    Reference,
    /// Cross-agent shared memory for swarm teams.
    Team,
    /// Per-agent persistent memory within a swarm.
    Agent,
}

impl MemoryType {
    /// Parse a memory type string (case-insensitive).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "user" => Some(Self::User),
            "feedback" => Some(Self::Feedback),
            "project" => Some(Self::Project),
            "reference" => Some(Self::Reference),
            "team" => Some(Self::Team),
            "agent" => Some(Self::Agent),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Feedback => "feedback",
            Self::Project => "project",
            Self::Reference => "reference",
            Self::Team => "team",
            Self::Agent => "agent",
        }
    }
}

// ── Memory header (frontmatter metadata) ────────────────────────────────────

#[derive(Debug, Clone)]
pub struct MemoryHeader {
    pub filename: String,
    pub file_path: PathBuf,
    pub mtime: SystemTime,
    pub name: Option<String>,
    pub description: Option<String>,
    pub memory_type: Option<MemoryType>,
}

// ── Memory entry (header + content) ─────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct MemoryEntry {
    pub header: MemoryHeader,
    /// Body text after the frontmatter (possibly truncated).
    pub content: String,
    pub truncated: bool,
}

// ── Frontmatter parsing ──────────────────────────────────────────────────────

/// Extract YAML frontmatter from `---\n...\n---` at the start of a file.
/// Returns `(frontmatter_lines, body)`.
fn parse_frontmatter(text: &str) -> (Vec<String>, &str) {
    let Some(rest) = text.strip_prefix("---") else {
        return (Vec::new(), text);
    };
    // Accept `---\n` or `---\r\n`
    let rest = rest.trim_start_matches('\n').trim_start_matches('\r');
    let Some(end) = rest.find("\n---") else {
        return (Vec::new(), text);
    };
    let fm = &rest[..end];
    let body_start = end + 4; // skip `\n---`
    let body = if body_start <= rest.len() {
        rest[body_start..]
            .trim_start_matches('\n')
            .trim_start_matches('\r')
    } else {
        ""
    };
    let lines: Vec<String> = fm.lines().map(|l| l.to_string()).collect();
    (lines, body)
}

/// Parse a simple YAML key: value line.
fn parse_yaml_kv(line: &str) -> Option<(&str, &str)> {
    let (k, v) = line.split_once(':')?;
    Some((k.trim(), v.trim()))
}

fn parse_header_from_frontmatter(
    lines: &[String],
) -> (Option<MemoryType>, Option<String>, Option<String>) {
    let mut mem_type = None;
    let mut description = None;
    let mut name = None;
    for line in lines {
        if let Some((k, v)) = parse_yaml_kv(line) {
            match k {
                "type" => mem_type = MemoryType::parse(v),
                "description" => description = Some(v.to_string()),
                "name" => name = Some(v.to_string()),
                _ => {}
            }
        }
    }
    (mem_type, description, name)
}

// ── Directory scanning ───────────────────────────────────────────────────────

/// Scan a directory for `*.md` files (excluding `MEMORY.md` index files).
/// Returns headers sorted newest-first, capped at `MAX_MEMORY_FILES`.
pub fn scan_memory_dir(dir: &Path) -> Vec<MemoryHeader> {
    let mut headers = Vec::new();

    let walk = walkdir::WalkDir::new(dir)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_type().is_file()
                && e.path().extension().is_some_and(|x| x == "md")
                && e.file_name() != "MEMORY.md"
        });

    for entry in walk {
        let path = entry.path().to_path_buf();
        let filename = path
            .strip_prefix(dir)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| {
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string()
            });

        let mtime = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .unwrap_or(SystemTime::UNIX_EPOCH);

        // Read first 30 lines for frontmatter only
        let preview = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                warn!("Skipped unreadable memory file {}: {}", path.display(), e);
                continue;
            }
        };
        let first_30: String = preview.lines().take(30).collect::<Vec<_>>().join("\n");
        let (fm_lines, _) = parse_frontmatter(&first_30);
        let (mem_type, description, name) = parse_header_from_frontmatter(&fm_lines);

        headers.push(MemoryHeader {
            filename,
            file_path: path,
            mtime,
            name,
            description,
            memory_type: mem_type,
        });
    }

    headers.sort_by(|a, b| b.mtime.cmp(&a.mtime));
    headers.truncate(MAX_MEMORY_FILES);
    headers
}

/// Find all memory directories to scan (in priority order):
///   1. `~/.claude/projects/<sanitized-git-root>/memory/`  (project-isolated)
///   2. `<cwd>/.claude/memory/`                            (in-repo project-scoped)
///   3. `~/.claude/memory/`                                (legacy user-global)
pub fn memory_dirs(cwd: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    // 1. Project-isolated: ~/.claude/projects/<key>/memory/
    if let Some(project_dir) = project_isolated_memory_dir(cwd) {
        if project_dir.exists() {
            dirs.push(project_dir);
        }
    }

    // 2. In-repo: <cwd>/.claude/memory/
    let project = cwd.join(".claude").join("memory");
    if project.exists() && !dirs.contains(&project) {
        dirs.push(project);
    }

    // 3. Legacy global: ~/.claude/memory/
    if let Some(home) = dirs::home_dir() {
        let p = home.join(".claude").join("memory");
        if p.exists() && !dirs.contains(&p) {
            dirs.push(p);
        }
    }

    dirs
}

/// Compute the project-isolated memory directory path.
///
/// Pattern: `~/.claude/projects/<sanitized-git-root>/memory/`
/// Falls back to `<sanitized-cwd>` if not inside a git repo.
fn project_isolated_memory_dir(cwd: &Path) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let base = crate::git_util::find_git_root(cwd).unwrap_or_else(|| cwd.to_path_buf());
    let key = crate::git_util::sanitize_path_key(&base.to_string_lossy());
    Some(
        home.join(".claude")
            .join("projects")
            .join(key)
            .join("memory"),
    )
}

/// Returns the primary memory directory path for behavioral prompt injection.
///
/// Prefers the project-isolated directory (`~/.claude/projects/<key>/memory/`),
/// falling back to in-repo `.claude/memory/`, then legacy global.
/// Creates the directory if it doesn't exist yet (so the model can write immediately).
pub fn primary_memory_dir(cwd: &Path) -> Option<PathBuf> {
    // 1. Project-isolated (preferred)
    if let Some(isolated) = project_isolated_memory_dir(cwd) {
        if !isolated.exists() {
            if let Err(e) = std::fs::create_dir_all(&isolated) {
                warn!("Failed to create memory dir {:?}: {}", isolated, e);
            }
        }
        if isolated.exists() {
            return Some(isolated);
        }
    }

    // 2. In-repo project dir
    let project = cwd.join(".claude").join("memory");
    if !project.exists() {
        if let Err(e) = std::fs::create_dir_all(&project) {
            warn!("Failed to create memory dir {:?}: {}", project, e);
        }
    }
    if project.exists() {
        return Some(project);
    }

    // 3. Legacy global
    if let Some(home) = dirs::home_dir() {
        let user_dir = home.join(".claude").join("memory");
        if user_dir.exists() {
            return Some(user_dir);
        }
    }
    None
}

// ── Reading memory content ───────────────────────────────────────────────────

/// Read the body of a memory file (after frontmatter), truncated to limit.
pub fn read_memory_body(path: &Path) -> (String, bool) {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            warn!("Failed to read memory file {:?}: {}", path, e);
            return (String::new(), false);
        }
    };
    let (_, body) = parse_frontmatter(&text);
    if body.len() > MAX_MEMORY_BYTES_PER_FILE {
        // Find a valid UTF-8 char boundary at or before the limit
        let mut end = MAX_MEMORY_BYTES_PER_FILE;
        while !body.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        (body[..end].to_string(), true)
    } else {
        (body.to_string(), false)
    }
}

// ── Human-readable age ───────────────────────────────────────────────────────

fn human_age(mtime: SystemTime) -> String {
    let elapsed = mtime.elapsed().unwrap_or_default();
    let secs = elapsed.as_secs();
    if secs < 60 {
        "just now".to_string()
    } else if secs < 3600 {
        format!("{} min ago", secs / 60)
    } else if secs < 86400 {
        format!("{} hr ago", secs / 3600)
    } else {
        format!("{} days ago", secs / 86400)
    }
}

// ── Public API ───────────────────────────────────────────────────────────────

/// Load all available memories and format them as a block for injection into
/// the system prompt.  Returns `None` if no memory files are found.
pub fn load_memories_for_prompt(cwd: &Path) -> Option<String> {
    let dirs = memory_dirs(cwd);
    if dirs.is_empty() {
        return None;
    }

    let mut all_headers: Vec<MemoryHeader> = Vec::new();
    for dir in &dirs {
        all_headers.extend(scan_memory_dir(dir));
    }
    // Re-sort globally and cap
    all_headers.sort_by(|a, b| b.mtime.cmp(&a.mtime));
    all_headers.truncate(MAX_MEMORY_FILES);

    if all_headers.is_empty() {
        return None;
    }

    let mut result = String::from("<memory>\n");
    result.push_str("The following memory files provide relevant context:\n\n");

    // Manifest section
    result.push_str("## Memory Files\n");
    for h in &all_headers {
        let tag = h
            .memory_type
            .as_ref()
            .map(|t| format!("[{}] ", t.as_str()))
            .unwrap_or_default();
        let age = human_age(h.mtime);
        if let Some(ref desc) = h.description {
            result.push_str(&format!("- {}{} ({}): {}\n", tag, h.filename, age, desc));
        } else {
            result.push_str(&format!("- {}{} ({})\n", tag, h.filename, age));
        }
    }

    // Content section
    let mut total_bytes = 0usize;
    result.push_str("\n## Memory Contents\n\n");

    for h in &all_headers {
        if total_bytes >= MAX_TOTAL_MEMORY_BYTES {
            result
                .push_str("\n> Additional memory files were omitted (context budget exceeded).\n");
            break;
        }

        let age = human_age(h.mtime);
        let header_line = format!("### Memory (saved {}): {}\n\n", age, h.filename);
        let (body, truncated) = read_memory_body(&h.file_path);

        result.push_str(&header_line);
        result.push_str(&body);
        if truncated {
            result.push_str(&format!(
                "\n\n> This memory file was truncated (>{} bytes). Use FileRead to view the full file.\n",
                MAX_MEMORY_BYTES_PER_FILE
            ));
        }
        result.push('\n');

        total_bytes += body.len();
    }

    result.push_str("</memory>\n");
    Some(result)
}

/// List memory headers (for `/memory list` CLI command).
pub fn list_memory_files(cwd: &Path) -> Vec<MemoryHeader> {
    let dirs = memory_dirs(cwd);
    let mut all: Vec<MemoryHeader> = dirs.iter().flat_map(|d| scan_memory_dir(d)).collect();
    all.sort_by(|a, b| b.mtime.cmp(&a.mtime));
    all
}

/// Return the primary user memory directory (creates it if missing).
pub fn ensure_user_memory_dir() -> anyhow::Result<PathBuf> {
    let dir = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("Cannot locate home directory"))?
        .join(".claude")
        .join("memory");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

// ── Pruning ──────────────────────────────────────────────────────────────────

/// Configuration for memory pruning.
#[derive(Debug, Clone)]
pub struct PruneConfig {
    /// Delete memories older than this many days (None = no age limit).
    pub max_age_days: Option<u64>,
    /// Keep at most this many files (delete oldest if exceeded).
    pub max_total_files: usize,
    /// Keep at most this many total bytes (delete oldest if exceeded).
    pub max_total_bytes: usize,
    /// Similarity threshold (0.0-1.0) for deduplication. 0.85 is a good default.
    pub similarity_threshold: f64,
}

impl Default for PruneConfig {
    fn default() -> Self {
        Self {
            max_age_days: Some(90),
            max_total_files: 200,
            max_total_bytes: 500_000,
            similarity_threshold: 0.85,
        }
    }
}

/// Prune memories in a directory according to the config.
///
/// Applies three strategies in order:
/// 1. Age pruning — delete memories older than `max_age_days`
/// 2. Quota pruning — delete oldest memories if file count or total bytes exceeds limits
/// 3. Deduplication — delete redundant memories with high textual similarity
///
/// Returns `(files_deleted, bytes_freed)`.
pub fn prune_memories(dir: &Path, config: &PruneConfig) -> std::io::Result<(usize, u64)> {
    let mut headers = scan_memory_dir(dir);
    if headers.is_empty() {
        return Ok((0, 0));
    }

    // Sort oldest-first for pruning order
    headers.sort_by(|a, b| a.mtime.cmp(&b.mtime));

    let mut deleted = 0usize;
    let mut bytes_freed = 0u64;
    let mut survivors: Vec<MemoryHeader> = Vec::new();

    // ── 1. Age pruning ───────────────────────────────────────────────
    if let Some(max_days) = config.max_age_days {
        let cutoff = SystemTime::now() - std::time::Duration::from_secs(max_days * 86400);
        for h in headers {
            if h.mtime < cutoff {
                if let Ok(meta) = std::fs::metadata(&h.file_path) {
                    bytes_freed += meta.len();
                }
                let _ = std::fs::remove_file(&h.file_path);
                deleted += 1;
            } else {
                survivors.push(h);
            }
        }
    } else {
        survivors = headers;
    }

    // ── 2. Quota pruning ─────────────────────────────────────────────
    let mut total_bytes: usize = survivors
        .iter()
        .filter_map(|h| {
            std::fs::metadata(&h.file_path)
                .ok()
                .map(|m| m.len() as usize)
        })
        .sum();

    while survivors.len() > config.max_total_files || total_bytes > config.max_total_bytes {
        if let Some(oldest) = survivors.first() {
            if let Ok(meta) = std::fs::metadata(&oldest.file_path) {
                bytes_freed += meta.len();
                total_bytes = total_bytes.saturating_sub(meta.len() as usize);
            }
            let _ = std::fs::remove_file(&oldest.file_path);
            deleted += 1;
            survivors.remove(0);
        } else {
            break;
        }
    }

    // ── 3. Deduplication ─────────────────────────────────────────────
    let dup_groups = find_duplicate_groups(&survivors, config.similarity_threshold);
    let mut keep: Vec<bool> = vec![true; survivors.len()];

    for group in &dup_groups {
        // Keep the newest (last in the oldest-first sorted list) in each group
        if group.len() > 1 {
            let newest_idx = *group.iter().max().unwrap();
            for &idx in group {
                if idx != newest_idx {
                    keep[idx] = false;
                }
            }
        }
    }

    for (i, h) in survivors.into_iter().enumerate() {
        if !keep[i] {
            if let Ok(meta) = std::fs::metadata(&h.file_path) {
                bytes_freed += meta.len();
            }
            let _ = std::fs::remove_file(&h.file_path);
            deleted += 1;
        }
    }

    // Re-update index if anything was deleted
    if deleted > 0 {
        let _ = update_memory_index(dir);
    }

    Ok((deleted, bytes_freed))
}

/// Compute Jaccard similarity between two strings using word sets.
fn jaccard_similarity(a: &str, b: &str) -> f64 {
    let set_a: std::collections::HashSet<String> = a
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_string())
        .collect();
    let set_b: std::collections::HashSet<String> = b
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_string())
        .collect();

    if set_a.is_empty() && set_b.is_empty() {
        return 1.0;
    }
    if set_a.is_empty() || set_b.is_empty() {
        return 0.0;
    }

    let intersection: usize = set_a.intersection(&set_b).count();
    let union: usize = set_a.union(&set_b).count();

    intersection as f64 / union as f64
}

/// Find groups of duplicate memories using Jaccard similarity.
///
/// Returns groups of indices into the `headers` slice. Each group contains
/// memories that are pairwise similar (transitive closure).
pub fn find_duplicate_groups(headers: &[MemoryHeader], threshold: f64) -> Vec<Vec<usize>> {
    if headers.len() < 2 {
        return Vec::new();
    }

    // Read all bodies first
    let bodies: Vec<String> = headers
        .iter()
        .map(|h| read_memory_body(&h.file_path).0)
        .collect();

    // Build adjacency list
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); headers.len()];
    for i in 0..headers.len() {
        for j in (i + 1)..headers.len() {
            let sim = jaccard_similarity(&bodies[i], &bodies[j]);
            if sim >= threshold {
                adj[i].push(j);
                adj[j].push(i);
            }
        }
    }

    // Find connected components via DFS
    let mut visited = vec![false; headers.len()];
    let mut groups: Vec<Vec<usize>> = Vec::new();

    for start in 0..headers.len() {
        if visited[start] {
            continue;
        }
        let mut stack = vec![start];
        let mut group = Vec::new();
        visited[start] = true;

        while let Some(node) = stack.pop() {
            group.push(node);
            for &neighbor in &adj[node] {
                if !visited[neighbor] {
                    visited[neighbor] = true;
                    stack.push(neighbor);
                }
            }
        }

        if group.len() > 1 {
            groups.push(group);
        }
    }

    groups
}

// ── MEMORY.md Index Management (TS parity: memdir.ts) ────────────────────────

/// Format a one-line manifest entry for a memory header.
///
/// TS format: `- [Title](file.md) — one-line description`
fn format_manifest_entry(h: &MemoryHeader) -> String {
    let title = h
        .name
        .as_deref()
        .or(h.description.as_deref())
        .unwrap_or(&h.filename);
    let desc_part = h
        .description
        .as_deref()
        .map(|d| format!(" — {}", d))
        .unwrap_or_default();
    format!("- [{}]({}){}", title, h.filename, desc_part)
}

/// Format the complete memory manifest from a list of headers.
///
/// Returns the index content (one line per memory file, newest first).
/// Used for both MEMORY.md file content and system prompt injection.
pub fn format_memory_manifest(headers: &[MemoryHeader]) -> String {
    let mut lines: Vec<String> = Vec::with_capacity(headers.len());
    for h in headers {
        lines.push(format_manifest_entry(h));
    }
    lines.join("\n")
}

/// Truncate manifest text to stay within MEMORY.md limits.
///
/// TS parity: max 200 lines, 25KB. Appends warning if truncated.
pub fn truncate_manifest(manifest: &str) -> String {
    let lines: Vec<&str> = manifest.lines().collect();
    let line_count = lines.len();

    // Line truncation
    let line_limited: Vec<&str> = if line_count > MAX_ENTRYPOINT_LINES {
        lines[..MAX_ENTRYPOINT_LINES].to_vec()
    } else {
        lines
    };

    let mut result = line_limited.join("\n");

    // Byte truncation
    if result.len() > MAX_ENTRYPOINT_BYTES {
        let truncated = &result[..MAX_ENTRYPOINT_BYTES];
        let end = truncated.rfind('\n').unwrap_or(MAX_ENTRYPOINT_BYTES);
        result = result[..end].to_string();
    }

    let was_truncated = line_count > MAX_ENTRYPOINT_LINES || manifest.len() > MAX_ENTRYPOINT_BYTES;
    if was_truncated {
        result.push_str("\n\n> ⚠️ Memory index truncated. Use FileRead on individual memory files for full content.");
    }

    result
}

/// Update (or create) the MEMORY.md index file in the given memory directory.
///
/// Scans the directory for `.md` files, builds the manifest, writes it out.
/// Returns the number of indexed entries.
pub fn update_memory_index(memory_dir: &Path) -> std::io::Result<usize> {
    let headers = scan_memory_dir(memory_dir);
    let manifest = format_memory_manifest(&headers);
    let truncated = truncate_manifest(&manifest);

    let index_path = memory_dir.join(MEMORY_INDEX_FILENAME);
    std::fs::write(&index_path, &truncated)?;

    Ok(headers.len())
}

/// Read and return the MEMORY.md index contents, if it exists and is non-empty.
pub fn read_memory_index(memory_dir: &Path) -> Option<String> {
    let index_path = memory_dir.join(MEMORY_INDEX_FILENAME);
    match std::fs::read_to_string(&index_path) {
        Ok(content) if !content.trim().is_empty() => Some(content),
        _ => None,
    }
}

/// Write a single memory file with proper frontmatter format.
///
/// Creates `<memory_dir>/<filename>` with YAML frontmatter containing
/// name, description, and type fields. Updates the MEMORY.md index afterwards.
pub fn write_memory_file(
    memory_dir: &Path,
    filename: &str,
    name: &str,
    description: &str,
    memory_type: MemoryType,
    content: &str,
) -> std::io::Result<PathBuf> {
    let path = memory_dir.join(filename);
    let file_content = format!(
        "---\nname: {}\ndescription: {}\ntype: {}\n---\n\n{}",
        name,
        description,
        memory_type.as_str(),
        content
    );
    std::fs::write(&path, &file_content)?;

    // Auto-update the MEMORY.md index
    let _ = update_memory_index(memory_dir);

    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // ── MemoryType::parse ─────────────────────────────────────────────

    #[test]
    fn memory_type_from_str_valid() {
        assert_eq!(MemoryType::parse("user"), Some(MemoryType::User));
        assert_eq!(MemoryType::parse("feedback"), Some(MemoryType::Feedback));
        assert_eq!(MemoryType::parse("project"), Some(MemoryType::Project));
        assert_eq!(MemoryType::parse("reference"), Some(MemoryType::Reference));
    }

    #[test]
    fn memory_type_from_str_team_and_agent() {
        assert_eq!(MemoryType::parse("team"), Some(MemoryType::Team));
        assert_eq!(MemoryType::parse("agent"), Some(MemoryType::Agent));
        assert_eq!(MemoryType::parse("TEAM"), Some(MemoryType::Team));
        assert_eq!(MemoryType::parse("AGENT"), Some(MemoryType::Agent));
    }

    #[test]
    fn memory_type_from_str_invalid() {
        assert_eq!(MemoryType::parse("unknown"), None);
        assert_eq!(MemoryType::parse(""), None);
        assert_eq!(MemoryType::parse("  "), None);
    }

    #[test]
    fn memory_type_from_str_case_insensitive() {
        assert_eq!(MemoryType::parse("User"), Some(MemoryType::User));
        assert_eq!(MemoryType::parse("FEEDBACK"), Some(MemoryType::Feedback));
        assert_eq!(MemoryType::parse("Project"), Some(MemoryType::Project));
        assert_eq!(MemoryType::parse("REFERENCE"), Some(MemoryType::Reference));
    }

    // ── MemoryType::as_str roundtrip ─────────────────────────────────────

    #[test]
    fn memory_type_as_str_roundtrip() {
        for variant in [
            MemoryType::User,
            MemoryType::Feedback,
            MemoryType::Project,
            MemoryType::Reference,
            MemoryType::Team,
            MemoryType::Agent,
        ] {
            let s = variant.as_str();
            let back = MemoryType::parse(s).expect("roundtrip should succeed");
            assert_eq!(back, variant);
        }
    }

    // ── parse_frontmatter ────────────────────────────────────────────────

    #[test]
    fn parse_frontmatter_with_valid_fm() {
        let text = "---\ntype: user\ndescription: hello\n---\nBody content here";
        let (lines, body) = parse_frontmatter(text);
        assert_eq!(lines, vec!["type: user", "description: hello"]);
        assert_eq!(body, "Body content here");
    }

    #[test]
    fn parse_frontmatter_no_fm() {
        let text = "Just some body text\nwith multiple lines";
        let (lines, body) = parse_frontmatter(text);
        assert!(lines.is_empty());
        assert_eq!(body, text);
    }

    #[test]
    fn parse_frontmatter_unclosed() {
        let text = "---\ntype: user\nno closing marker";
        let (lines, body) = parse_frontmatter(text);
        assert!(lines.is_empty());
        assert_eq!(body, text);
    }

    #[test]
    fn parse_frontmatter_empty_body() {
        let text = "---\ntype: project\n---\n";
        let (lines, body) = parse_frontmatter(text);
        assert_eq!(lines, vec!["type: project"]);
        assert!(body.is_empty() || body.trim().is_empty());
    }

    // ── parse_yaml_kv ────────────────────────────────────────────────────

    #[test]
    fn parse_yaml_kv_valid() {
        assert_eq!(parse_yaml_kv("type: user"), Some(("type", "user")));
        assert_eq!(
            parse_yaml_kv("description: some text"),
            Some(("description", "some text"))
        );
        // Extra whitespace around key/value
        assert_eq!(parse_yaml_kv("  key : value  "), Some(("key", "value")));
    }

    #[test]
    fn parse_yaml_kv_no_colon() {
        assert_eq!(parse_yaml_kv("no colon here"), None);
        assert_eq!(parse_yaml_kv(""), None);
    }

    // ── parse_header_from_frontmatter ────────────────────────────────────

    #[test]
    fn parse_header_type_and_description() {
        let lines = vec![
            "type: feedback".to_string(),
            "description: My memory note".to_string(),
            "name: My Note".to_string(),
        ];
        let (mt, desc, name) = parse_header_from_frontmatter(&lines);
        assert_eq!(mt, Some(MemoryType::Feedback));
        assert_eq!(desc.as_deref(), Some("My memory note"));
        assert_eq!(name.as_deref(), Some("My Note"));
    }

    #[test]
    fn parse_header_unknown_type() {
        let lines = vec!["type: banana".to_string()];
        let (mt, desc, name) = parse_header_from_frontmatter(&lines);
        assert_eq!(mt, None);
        assert_eq!(desc, None);
        assert_eq!(name, None);
    }

    #[test]
    fn parse_header_empty_lines() {
        let (mt, desc, name) = parse_header_from_frontmatter(&[]);
        assert_eq!(mt, None);
        assert_eq!(desc, None);
        assert_eq!(name, None);
    }

    // ── human_age ────────────────────────────────────────────────────────

    #[test]
    fn human_age_just_now() {
        let now = SystemTime::now();
        assert_eq!(human_age(now), "just now");
    }

    #[test]
    fn human_age_minutes() {
        let t = SystemTime::now() - Duration::from_secs(5 * 60);
        assert_eq!(human_age(t), "5 min ago");
    }

    #[test]
    fn human_age_hours() {
        let t = SystemTime::now() - Duration::from_secs(2 * 3600);
        assert_eq!(human_age(t), "2 hr ago");
    }

    #[test]
    fn human_age_days() {
        let t = SystemTime::now() - Duration::from_secs(3 * 86400);
        assert_eq!(human_age(t), "3 days ago");
    }

    // ── scan_memory_dir ──────────────────────────────────────────────────

    #[test]
    fn scan_memory_dir_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let headers = scan_memory_dir(tmp.path());
        assert!(headers.is_empty());
    }

    #[test]
    fn scan_memory_dir_with_files() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("note1.md"),
            "---\ntype: user\ndescription: First note\n---\nHello world",
        )
        .unwrap();
        std::fs::write(tmp.path().join("note2.md"), "No frontmatter body").unwrap();

        let headers = scan_memory_dir(tmp.path());
        assert_eq!(headers.len(), 2);

        // Find the one with frontmatter
        let with_fm = headers.iter().find(|h| h.filename == "note1.md").unwrap();
        assert_eq!(with_fm.memory_type, Some(MemoryType::User));
        assert_eq!(with_fm.description.as_deref(), Some("First note"));

        // The one without frontmatter
        let without_fm = headers.iter().find(|h| h.filename == "note2.md").unwrap();
        assert_eq!(without_fm.memory_type, None);
        assert_eq!(without_fm.description, None);
    }

    #[test]
    fn scan_memory_dir_skips_memory_md() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("MEMORY.md"), "Index file").unwrap();
        std::fs::write(tmp.path().join("real.md"), "Content").unwrap();

        let headers = scan_memory_dir(tmp.path());
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].filename, "real.md");
    }

    #[test]
    fn scan_memory_dir_skips_non_md_files() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("note.md"), "Markdown").unwrap();
        std::fs::write(tmp.path().join("data.txt"), "Text").unwrap();
        std::fs::write(tmp.path().join("config.json"), "{}").unwrap();

        let headers = scan_memory_dir(tmp.path());
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].filename, "note.md");
    }

    // ── load_memories_for_prompt (via a fake project dir) ────────────────

    #[test]
    fn load_memories_for_prompt_empty() {
        let tmp = tempfile::tempdir().unwrap();
        // No .claude/memory/ directory ⇒ None
        let result = load_memories_for_prompt(tmp.path());
        assert!(result.is_none());
    }

    #[test]
    fn load_memories_for_prompt_with_files() {
        let tmp = tempfile::tempdir().unwrap();
        let mem_dir = tmp.path().join(".claude").join("memory");
        std::fs::create_dir_all(&mem_dir).unwrap();
        std::fs::write(
            mem_dir.join("greeting.md"),
            "---\ntype: project\ndescription: A greeting\n---\nHello from memory!",
        )
        .unwrap();

        let result = load_memories_for_prompt(tmp.path());
        let text = result.expect("should return Some for non-empty memory dir");

        assert!(text.starts_with("<memory>\n"));
        assert!(text.ends_with("</memory>\n"));
        assert!(text.contains("greeting.md"));
        assert!(text.contains("[project]"));
        assert!(text.contains("A greeting"));
        assert!(text.contains("Hello from memory!"));
    }

    // ── read_memory_body (indirectly via load_memories_for_prompt) ───────

    #[test]
    fn load_memories_truncates_large_file() {
        let tmp = tempfile::tempdir().unwrap();
        let mem_dir = tmp.path().join(".claude").join("memory");
        std::fs::create_dir_all(&mem_dir).unwrap();

        // Create a file whose body exceeds MAX_MEMORY_BYTES_PER_FILE (10_000)
        let big_body = "x".repeat(15_000);
        let content = format!("---\ntype: user\n---\n{}", big_body);
        std::fs::write(mem_dir.join("big.md"), content).unwrap();

        let result = load_memories_for_prompt(tmp.path()).unwrap();
        assert!(result.contains("truncated"));
        assert!(result.contains(">10000 bytes"));
    }

    // ── primary_memory_dir ─────────────────────────────────────────────

    #[test]
    fn primary_memory_dir_creates_some_dir() {
        // primary_memory_dir tries project-isolated first (needs home + git root),
        // then falls back to in-repo .claude/memory/
        let tmp = tempfile::tempdir().unwrap();
        let result = primary_memory_dir(tmp.path());
        // Should always return Some (it creates the directory)
        assert!(result.is_some());
        let dir = result.unwrap();
        assert!(dir.exists());
        // The path should end with "memory"
        assert_eq!(dir.file_name().unwrap().to_str().unwrap(), "memory");
    }

    #[test]
    fn primary_memory_dir_fallback_to_in_repo() {
        // When project-isolated dir can't be created (e.g., no home),
        // falls back to in-repo <cwd>/.claude/memory/
        let tmp = tempfile::tempdir().unwrap();
        let in_repo = tmp.path().join(".claude").join("memory");
        std::fs::create_dir_all(&in_repo).unwrap();

        let result = primary_memory_dir(tmp.path());
        assert!(result.is_some());
        // Should return some valid memory dir
        assert!(result.unwrap().exists());
    }

    // ── project_isolated_memory_dir ─────────────────────────────────────

    #[test]
    fn project_isolated_dir_contains_sanitized_key() {
        let tmp = tempfile::tempdir().unwrap();
        let result = super::project_isolated_memory_dir(tmp.path());
        if let Some(dir) = result {
            // Should contain "projects" in the path
            let path_str = dir.to_string_lossy();
            assert!(path_str.contains("projects"));
            assert!(path_str.contains("memory"));
        }
        // If home_dir() is None (rare), result is None — that's ok
    }

    // ── memory_dirs with project-isolated ─────────────────────────────────

    #[test]
    fn memory_dirs_includes_in_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let in_repo = tmp.path().join(".claude").join("memory");
        std::fs::create_dir_all(&in_repo).unwrap();

        let dirs = memory_dirs(tmp.path());
        assert!(dirs.contains(&in_repo));
    }

    // ── MEMORY.md index management ──────────────────────────────────────

    #[test]
    fn format_manifest_entry_with_name_and_desc() {
        let h = MemoryHeader {
            filename: "user_role.md".to_string(),
            file_path: PathBuf::from("/tmp/user_role.md"),
            mtime: SystemTime::now(),
            name: Some("User's Role".to_string()),
            description: Some("Data scientist focused on logging".to_string()),
            memory_type: Some(MemoryType::User),
        };
        let entry = format_manifest_entry(&h);
        assert_eq!(
            entry,
            "- [User's Role](user_role.md) — Data scientist focused on logging"
        );
    }

    #[test]
    fn format_manifest_entry_no_name_uses_description() {
        let h = MemoryHeader {
            filename: "note.md".to_string(),
            file_path: PathBuf::from("/tmp/note.md"),
            mtime: SystemTime::now(),
            name: None,
            description: Some("A simple note".to_string()),
            memory_type: None,
        };
        let entry = format_manifest_entry(&h);
        assert_eq!(entry, "- [A simple note](note.md) — A simple note");
    }

    #[test]
    fn format_manifest_entry_no_name_no_desc() {
        let h = MemoryHeader {
            filename: "orphan.md".to_string(),
            file_path: PathBuf::from("/tmp/orphan.md"),
            mtime: SystemTime::now(),
            name: None,
            description: None,
            memory_type: None,
        };
        let entry = format_manifest_entry(&h);
        assert_eq!(entry, "- [orphan.md](orphan.md)");
    }

    #[test]
    fn format_memory_manifest_multiple() {
        let headers = vec![
            MemoryHeader {
                filename: "a.md".to_string(),
                file_path: PathBuf::from("a.md"),
                mtime: SystemTime::now(),
                name: Some("Alpha".to_string()),
                description: Some("First".to_string()),
                memory_type: Some(MemoryType::User),
            },
            MemoryHeader {
                filename: "b.md".to_string(),
                file_path: PathBuf::from("b.md"),
                mtime: SystemTime::now(),
                name: Some("Beta".to_string()),
                description: None,
                memory_type: Some(MemoryType::Project),
            },
        ];
        let manifest = format_memory_manifest(&headers);
        assert!(manifest.contains("- [Alpha](a.md) — First"));
        assert!(manifest.contains("- [Beta](b.md)"));
        assert_eq!(manifest.lines().count(), 2);
    }

    #[test]
    fn truncate_manifest_within_limits() {
        let manifest = "- [A](a.md) — desc\n- [B](b.md) — desc2";
        let result = truncate_manifest(manifest);
        assert_eq!(result, manifest);
        assert!(!result.contains("truncated"));
    }

    #[test]
    fn truncate_manifest_over_line_limit() {
        let lines: Vec<String> = (0..250)
            .map(|i| format!("- [Item{}](item{}.md) — desc", i, i))
            .collect();
        let manifest = lines.join("\n");
        let result = truncate_manifest(&manifest);
        // Should be 200 lines + warning
        let result_lines: Vec<&str> = result.lines().collect();
        assert!(result_lines.len() <= 203); // 200 content + blank + warning
        assert!(result.contains("truncated"));
    }

    #[test]
    fn update_memory_index_creates_file() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("note1.md"),
            "---\nname: Note One\ndescription: First note\ntype: user\n---\nContent",
        )
        .unwrap();

        let count = update_memory_index(tmp.path()).unwrap();
        assert_eq!(count, 1);

        let index_path = tmp.path().join("MEMORY.md");
        assert!(index_path.exists());
        let content = std::fs::read_to_string(&index_path).unwrap();
        assert!(content.contains("[Note One]"));
        assert!(content.contains("note1.md"));
        assert!(content.contains("First note"));
    }

    #[test]
    fn read_memory_index_none_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(read_memory_index(tmp.path()).is_none());
    }

    #[test]
    fn read_memory_index_some_when_exists() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("MEMORY.md"), "- [A](a.md)").unwrap();
        let content = read_memory_index(tmp.path()).unwrap();
        assert!(content.contains("[A](a.md)"));
    }

    #[test]
    fn write_memory_file_creates_with_frontmatter() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_memory_file(
            tmp.path(),
            "test_mem.md",
            "Test Memory",
            "A test memory",
            MemoryType::Feedback,
            "Some important feedback",
        )
        .unwrap();

        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("name: Test Memory"));
        assert!(content.contains("description: A test memory"));
        assert!(content.contains("type: feedback"));
        assert!(content.contains("Some important feedback"));

        // Should also have created MEMORY.md index
        let index_path = tmp.path().join("MEMORY.md");
        assert!(index_path.exists());
        let index = std::fs::read_to_string(&index_path).unwrap();
        assert!(index.contains("test_mem.md"));
    }

    #[test]
    fn scan_memory_dir_parses_name_field() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("named.md"),
            "---\nname: My Named Memory\ntype: reference\n---\nContent",
        )
        .unwrap();

        let headers = scan_memory_dir(tmp.path());
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].name.as_deref(), Some("My Named Memory"));
        assert_eq!(headers[0].memory_type, Some(MemoryType::Reference));
    }

    // ── Pruning ────────────────────────────────────────────────────────

    #[test]
    fn prune_by_age_deletes_old_files() {
        let tmp = tempfile::tempdir().unwrap();
        let old = tmp.path().join("old.md");
        std::fs::write(&old, "---\ntype: user\n---\nVery old memory").unwrap();
        // Manually set mtime to 100 days ago
        let old_time = SystemTime::now() - Duration::from_secs(100 * 86400);
        let _ = filetime::set_file_mtime(&old, filetime::FileTime::from_system_time(old_time));

        let config = PruneConfig {
            max_age_days: Some(30),
            max_total_files: 200,
            max_total_bytes: 500_000,
            similarity_threshold: 0.85,
        };
        let (deleted, _) = prune_memories(tmp.path(), &config).unwrap();
        assert_eq!(deleted, 1);
        assert!(!old.exists());
    }

    #[test]
    fn prune_by_quota_keeps_newest() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..5 {
            let path = tmp.path().join(format!("note_{}.md", i));
            std::fs::write(&path, format!("---\ntype: user\n---\nNote {}", i)).unwrap();
            // Stagger mtimes
            let t = SystemTime::now() - Duration::from_secs((5 - i) as u64 * 60);
            let _ = filetime::set_file_mtime(&path, filetime::FileTime::from_system_time(t));
        }

        let config = PruneConfig {
            max_age_days: None,
            max_total_files: 3,
            max_total_bytes: 500_000,
            similarity_threshold: 0.85,
        };
        let (deleted, _) = prune_memories(tmp.path(), &config).unwrap();
        assert_eq!(deleted, 2);

        let remaining: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
            .filter(|e| e.file_name() != "MEMORY.md")
            .collect();
        assert_eq!(remaining.len(), 3);
    }

    #[test]
    fn prune_deduplication_removes_similar() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a.md");
        let b = tmp.path().join("b.md");
        std::fs::write(
            &a,
            "---\ntype: user\n---\nUser prefers Rust for all projects",
        )
        .unwrap();
        std::fs::write(
            &b,
            "---\ntype: user\n---\nUser prefers Rust for all coding work",
        )
        .unwrap();

        let config = PruneConfig {
            max_age_days: None,
            max_total_files: 200,
            max_total_bytes: 500_000,
            similarity_threshold: 0.5, // low threshold so they match
        };
        let (deleted, _) = prune_memories(tmp.path(), &config).unwrap();
        assert_eq!(deleted, 1); // one of the duplicates removed
    }

    #[test]
    fn prune_empty_dir_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let config = PruneConfig::default();
        let (deleted, _) = prune_memories(tmp.path(), &config).unwrap();
        assert_eq!(deleted, 0);
    }

    // ── Jaccard similarity ─────────────────────────────────────────────

    #[test]
    fn jaccard_identical_strings() {
        assert_eq!(super::jaccard_similarity("hello world", "hello world"), 1.0);
    }

    #[test]
    fn jaccard_completely_different() {
        let sim = super::jaccard_similarity("abc def", "xyz uvw");
        assert_eq!(sim, 0.0);
    }

    #[test]
    fn jaccard_partial_overlap() {
        let sim = super::jaccard_similarity("rust cargo clippy", "rust cargo build");
        assert!(sim > 0.3 && sim < 0.7);
    }

    #[test]
    fn jaccard_empty_strings() {
        assert_eq!(super::jaccard_similarity("", ""), 1.0);
        assert_eq!(super::jaccard_similarity("", "hello"), 0.0);
    }

    // ── Duplicate group finding ────────────────────────────────────────

    #[test]
    fn find_duplicate_groups_none_when_all_unique() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a.md");
        let b = tmp.path().join("b.md");
        std::fs::write(&a, "---\ntype: user\n---\nUser likes Rust").unwrap();
        std::fs::write(&b, "---\ntype: project\n---\nProject uses Python").unwrap();

        let headers = vec![
            MemoryHeader {
                filename: "a.md".to_string(),
                file_path: a,
                mtime: SystemTime::now(),
                name: None,
                description: None,
                memory_type: None,
            },
            MemoryHeader {
                filename: "b.md".to_string(),
                file_path: b,
                mtime: SystemTime::now(),
                name: None,
                description: None,
                memory_type: None,
            },
        ];
        let groups = find_duplicate_groups(&headers, 0.9);
        assert!(groups.is_empty());
    }

    #[test]
    fn find_duplicate_groups_finds_similar() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a.md");
        let b = tmp.path().join("b.md");
        std::fs::write(
            &a,
            "---\ntype: user\n---\nUser prefers Rust for all projects",
        )
        .unwrap();
        std::fs::write(
            &b,
            "---\ntype: user\n---\nUser prefers Rust for all coding work",
        )
        .unwrap();

        let headers = vec![
            MemoryHeader {
                filename: "a.md".to_string(),
                file_path: a,
                mtime: SystemTime::now(),
                name: None,
                description: None,
                memory_type: None,
            },
            MemoryHeader {
                filename: "b.md".to_string(),
                file_path: b,
                mtime: SystemTime::now(),
                name: None,
                description: None,
                memory_type: None,
            },
        ];
        let groups = find_duplicate_groups(&headers, 0.5);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), 2);
    }
}
