//! /memory command handler.

/// Handle /memory subcommands, returning output as a string for TUI display.
pub(crate) fn handle_memory_command_str(sub: &str, cwd: &std::path::Path) -> String {
    let parts: Vec<&str> = sub.splitn(2, ' ').collect();
    match parts.first().copied().unwrap_or("") {
        "" | "list" => {
            let files = clawed_core::memory::list_memory_files(cwd);
            if files.is_empty() {
                "No memory files found.\nCreate .md files in ~/.claude/memory/ or .claude/memory/ to use memory.".to_string()
            } else {
                let mut out = format!("Memory files ({}):", files.len());
                for f in &files {
                    let type_tag = f.memory_type.as_ref()
                        .map(|t| format!("[{}] ", t.as_str()))
                        .unwrap_or_default();
                    let desc = f.description.as_deref().unwrap_or("");
                    out.push_str(&format!("\n  {}{:<40} {}", type_tag, f.filename, desc));
                }
                out
            }
        }
        "open" => {
            let rel_path = parts.get(1).copied().unwrap_or("").trim();
            if rel_path.is_empty() {
                return "Usage: /memory open <filename>".to_string();
            }
            // Validate: reject path traversal attempts
            if rel_path.contains("..") || rel_path.starts_with('/') || rel_path.starts_with('\\') || rel_path.contains(':') {
                return "Invalid filename: must be a simple name without path separators or '..'".to_string();
            }
            // Try to find the file in memory dirs
            let mem_dirs = clawed_core::memory::memory_dirs(cwd);
            for dir in &mem_dirs {
                let p = dir.join(rel_path);
                // Verify resolved path stays inside the memory directory
                if let Ok(canonical) = p.canonicalize() {
                    if !canonical.starts_with(dir) {
                        continue;
                    }
                }
                if p.exists() {
                    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "notepad".into());
                    let _ = std::process::Command::new(&editor).arg(&p).status();
                    return format!("Opened {} in {}", rel_path, editor);
                }
            }
            format!("Memory file '{}' not found in any memory directory.", rel_path)
        }
        "add" => {
            let content = parts.get(1).copied().unwrap_or("").trim();
            if content.is_empty() {
                return "Usage: /memory add <content>".to_string();
            }
            // Write to project memory dir
            let mem_dir = cwd.join(".claude").join("memory");
            if let Err(e) = std::fs::create_dir_all(&mem_dir) {
                return format!("Failed to create memory dir: {e}");
            }
            // Auto-generate filename from content prefix
            let slug: String = content.chars()
                .take(40)
                .filter(|c| c.is_alphanumeric() || *c == ' ')
                .collect::<String>()
                .trim()
                .replace(' ', "_")
                .to_lowercase();
            let filename = format!("{}.md", if slug.is_empty() { "note" } else { &slug });
            let path = mem_dir.join(&filename);
            match std::fs::write(&path, content) {
                Ok(()) => format!("✓ Memory saved to {}", path.display()),
                Err(e) => format!("Failed to write memory: {e}"),
            }
        }
        other => {
            format!("Unknown memory subcommand: '{}'. Use list, open <file>, or add <content>.", other)
        }
    }
}

/// Handle /memory subcommands (prints to stdout — for REPL, not TUI).
pub(crate) fn handle_memory_command(sub: &str, cwd: &std::path::Path) {
    println!("{}", handle_memory_command_str(sub, cwd));
}
