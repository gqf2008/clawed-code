//! Memory maintenance — consolidation, synthesis, and periodic cleanup.
//!
//! These operations run after session end (or on explicit `/memory maintain`)
//! to keep the memory corpus high-quality:
//!
//! - **Consolidation**: merge related memories of the same type into higher-level summaries
//! - **Synthesis**: generate a project-level insight memory from the full corpus
//!
//! Both require an LLM call, so they are placed in `clawed-agent` (not `clawed-core`).

use std::collections::HashMap;
use std::path::Path;

use clawed_api::client::ApiClient;
use clawed_api::types::{MessagesRequest, ResponseContentBlock, SystemBlock};
use clawed_core::memory::{self, MemoryHeader, MemoryType};

// ── Consolidation ────────────────────────────────────────────────────────────

/// Consolidate memories by type: groups of 3+ memories of the same type are
/// sent to the LLM to produce a single higher-level memory.
///
/// Returns the number of consolidation groups processed.
pub async fn consolidate_memories(
    client: &ApiClient,
    model: &str,
    memory_dir: &Path,
) -> anyhow::Result<usize> {
    let headers = memory::scan_memory_dir(memory_dir);
    if headers.len() < 3 {
        return Ok(0);
    }

    // Group by memory type (None → "unknown")
    let mut by_type: HashMap<String, Vec<&MemoryHeader>> = HashMap::new();
    for h in &headers {
        let key = h.memory_type.as_ref().map(|t| t.as_str()).unwrap_or("unknown");
        by_type.entry(key.to_string()).or_default().push(h);
    }

    let mut groups_processed = 0usize;

    for (type_key, group) in by_type {
        if group.len() < 3 {
            continue;
        }

        // Build prompt from group contents
        let mut entries = Vec::new();
        for h in &group {
            let (body, _) = memory::read_memory_body(&h.file_path);
            let label = h.name.as_deref().unwrap_or(&h.filename);
            entries.push((label.to_string(), body));
        }

        let prompt = build_consolidation_prompt(&type_key, &entries);
        let summary = call_maintenance_llm(client, model, &prompt).await?;

        if summary.trim().len() < 20 {
            continue; // too short to be useful
        }

        // Delete old memories in this group
        for h in &group {
            let _ = std::fs::remove_file(&h.file_path);
        }

        // Write consolidated memory
        let filename = format!("consolidated-{}-{}.md", type_key, uuid::Uuid::new_v4());
        let mem_type = MemoryType::parse(&type_key).unwrap_or(MemoryType::Project);
        let _ = memory::write_memory_file(
            memory_dir,
            &filename,
            &format!("Consolidated {} memories", type_key),
            &format!("Merged {} {} memories", group.len(), type_key),
            mem_type,
            &summary,
        );

        groups_processed += 1;
    }

    Ok(groups_processed)
}

fn build_consolidation_prompt(mem_type: &str, entries: &[(String, String)]) -> String {
    let mut prompt = format!(
        "You are a memory consolidation assistant. \
        Below are {} memory entries of type '{}'. \
        Merge them into ONE coherent, higher-level memory. \
        Preserve all distinct facts, preferences, and decisions. \
        Remove redundancy and temporal noise (e.g., 'just now', 'today'). \
        Write in the third person, present tense. \
        Keep under 500 words.",
        entries.len(),
        mem_type
    );

    for (i, (label, body)) in entries.iter().enumerate() {
        prompt.push_str(&format!("\n\n--- Entry {}: {} ---\n{}", i + 1, label, body));
    }

    prompt.push_str(
        "\n\nRespond with ONLY the consolidated memory text. \
        No markdown fences, no preamble, no bullet points unless the original used them.",
    );

    prompt
}

// ── Synthesis ────────────────────────────────────────────────────────────────

/// Synthesize all memories into a single project-level insight.
///
/// Reads the full memory corpus, asks the LLM for a high-level summary of
/// what the project is about, what the user cares about, and what patterns
/// have emerged. Writes the result as a `project` memory file.
pub async fn synthesize_memories(
    client: &ApiClient,
    model: &str,
    memory_dir: &Path,
) -> anyhow::Result<()> {
    let headers = memory::scan_memory_dir(memory_dir);
    if headers.is_empty() {
        return Ok(());
    }

    let mut entries = Vec::new();
    for h in &headers {
        let (body, _) = memory::read_memory_body(&h.file_path);
        let mem_type = h.memory_type.as_ref().map(|t| t.as_str()).unwrap_or("unknown");
        entries.push((
            h.name.clone().unwrap_or_else(|| h.filename.clone()),
            mem_type.to_string(),
            body,
        ));
    }

    let prompt = build_synthesis_prompt(&entries);
    let insight = call_maintenance_llm(client, model, &prompt).await?;

    if insight.trim().len() < 30 {
        return Ok(()); // not useful
    }

    // Write as a project memory, overwriting any previous synthesis
    let filename = "synthesis-project-insight.md";
    let _ = memory::write_memory_file(
        memory_dir,
        filename,
        "Project Insight (auto-synthesized)",
        "High-level synthesis of all memories",
        MemoryType::Project,
        &insight,
    );

    Ok(())
}

fn build_synthesis_prompt(entries: &[(String, String, String)]) -> String {
    let mut prompt = String::from(
        "You are a project intelligence assistant. \
        Below are memories collected across sessions. \
        Synthesize them into a concise project insight (max 400 words) that answers:\n\
        1. What is this project about?\n\
        2. What does the user care most about?\n\
        3. What recurring patterns or decisions have emerged?\n\
        4. What should a new assistant know on day one?\n\n\
        Memories:\n",
    );

    for (label, mem_type, body) in entries {
        prompt.push_str(&format!(
            "\n--- [{} | {}] ---\n{}",
            label,
            mem_type,
            body.chars().take(800).collect::<String>()
        ));
    }

    prompt.push_str(
        "\n\nRespond with ONLY the synthesis text. No markdown fences, no preamble.",
    );

    prompt
}

// ── Shared LLM helper ────────────────────────────────────────────────────────

async fn call_maintenance_llm(
    client: &ApiClient,
    model: &str,
    prompt: &str,
) -> anyhow::Result<String> {
    let system = vec![SystemBlock {
        block_type: "text".into(),
        text: prompt.to_string(),
        cache_control: None,
    }];

    let request = MessagesRequest {
        model: model.to_string(),
        max_tokens: 4096,
        messages: Vec::new(),
        system: Some(system),
        tools: None,
        stream: false,
        stop_sequences: None,
        temperature: Some(0.3),
        top_p: None,
        thinking: None,
        tool_choice: None,
    };

    let response = client
        .messages(&request)
        .await
        .map_err(|e| anyhow::anyhow!("Memory maintenance LLM call failed: {}", e))?;

    let text: String = response
        .content
        .iter()
        .filter_map(|b| {
            if let ResponseContentBlock::Text { text } = b {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("");

    if text.trim().is_empty() {
        anyhow::bail!("Memory maintenance LLM returned empty response");
    }

    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_consolidation_prompt_includes_entries() {
        let entries = vec![
            ("A".into(), "User likes Rust".into()),
            ("B".into(), "User prefers cargo".into()),
        ];
        let p = build_consolidation_prompt("user", &entries);
        assert!(p.contains("memory consolidation assistant"));
        assert!(p.contains("User likes Rust"));
        assert!(p.contains("User prefers cargo"));
    }

    #[test]
    fn build_synthesis_prompt_includes_memories() {
        let entries = vec![(
            "Pref".into(),
            "user".into(),
            "User likes Rust".into(),
        )];
        let p = build_synthesis_prompt(&entries);
        assert!(p.contains("project intelligence assistant"));
        assert!(p.contains("User likes Rust"));
    }
}
