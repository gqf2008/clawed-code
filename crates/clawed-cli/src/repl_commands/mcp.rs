//! `/mcp` command handler — MCP server discovery and status.

use std::fmt::Write as _;

use clawed_tools::mcp::{discover_mcp_configs, load_mcp_configs};

/// Handle `/mcp [subcommand]`.
pub(crate) fn handle_mcp_command(sub: &str, cwd: &std::path::Path) {
    println!("{}", handle_mcp_command_str(sub, cwd));
}

pub(crate) fn handle_mcp_command_str(sub: &str, cwd: &std::path::Path) -> String {
    let sub = sub.trim();
    match sub {
        "" | "list" | "status" => show_mcp_status(cwd),
        "help" => "\x1b[1mMCP Server Management\x1b[0m\n\n  /mcp              Show discovered MCP servers\n  /mcp list         Same as /mcp\n  /mcp status       Same as /mcp\n\nMCP configs are loaded from:\n  1. <cwd>/.mcp.json         (project-level)\n  2. ~/.claude/.mcp.json      (user-level)".to_string(),
        _ => {
            let safe_sub: String = sub
                .chars()
                .filter(|c| !c.is_control() || *c == ' ')
                .take(50)
                .collect();
            format!("Unknown subcommand: /mcp {}. Try /mcp help", safe_sub)
        }
    }
}

fn show_mcp_status(cwd: &std::path::Path) -> String {
    let config_paths = discover_mcp_configs(cwd);

    if config_paths.is_empty() {
        return "\x1b[2mNo MCP server configs found.\x1b[0m\n\x1b[2mCreate .mcp.json in your project or ~/.claude/ to configure MCP servers.\x1b[0m\n\x1b[2mExample .mcp.json:\x1b[0m\n\x1b[2m{\x1b[0m\n\x1b[2m  \"mcpServers\": {\x1b[0m\n\x1b[2m    \"my-server\": {\x1b[0m\n\x1b[2m      \"command\": \"npx\",\x1b[0m\n\x1b[2m      \"args\": [\"-y\", \"@my-org/my-mcp-server\"]\x1b[0m\n\x1b[2m    }\x1b[0m\n\x1b[2m  }\x1b[0m\n\x1b[2m}\x1b[0m".to_string();
    }

    let mut out = String::from("\x1b[1mMCP Server Configs\x1b[0m\n\n");

    for path in &config_paths {
        let location = if path.starts_with(cwd) {
            "project"
        } else {
            "user"
        };
        let _ = writeln!(out, "\x1b[2m[{}] {}\x1b[0m", location, path.display());

        match load_mcp_configs(path) {
            Ok(configs) => {
                if configs.is_empty() {
                    out.push_str("  (no servers defined)\n");
                }
                for cfg in &configs {
                    let args_str = if cfg.args.is_empty() {
                        String::new()
                    } else {
                        format!(" {}", cfg.args.join(" "))
                    };
                    let _ = writeln!(out, "  \x1b[32m●\x1b[0m \x1b[1m{}\x1b[0m", cfg.name);
                    let _ = writeln!(out, "    command: {}{}", cfg.command, args_str);
                    if !cfg.env.is_empty() {
                        let env_keys: Vec<&str> = cfg.env.keys().map(|key| key.as_str()).collect();
                        let _ = writeln!(out, "    env: {}", env_keys.join(", "));
                    }
                }
            }
            Err(error) => {
                let _ = writeln!(out, "  \x1b[31m✗ Failed to load: {}\x1b[0m", error);
            }
        }
    }

    out.push_str("\n\x1b[2mMCP tools are injected into the system prompt at startup.\x1b[0m");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_show_mcp_no_configs() {
        let tmp = std::env::temp_dir().join("claude_test_mcp_cmd");
        std::fs::create_dir_all(&tmp).expect("failed to create temp dir");
        let output = show_mcp_status(&tmp);
        assert!(!output.is_empty());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_show_mcp_with_config() {
        let tmp = std::env::temp_dir().join("claude_test_mcp_cmd2");
        std::fs::create_dir_all(&tmp).expect("failed to create temp dir");
        let config = r#"{
            "mcpServers": {
                "test-server": {
                    "command": "node",
                    "args": ["server.js"],
                    "env": { "PORT": "3000" }
                }
            }
        }"#;
        std::fs::write(tmp.join(".mcp.json"), config).expect("failed to write config");
        let output = show_mcp_status(&tmp);
        assert!(output.contains("test-server"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_handle_mcp_unknown_sanitizes_input() {
        let output = handle_mcp_command_str("\x1b[31mevil\x1b[0m", &std::env::temp_dir());
        assert!(!output.contains('\x1b'));
        assert!(output.contains("evil"));
    }
}
