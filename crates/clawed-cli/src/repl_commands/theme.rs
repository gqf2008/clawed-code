//! `/theme` command handler.

use std::fmt::Write as _;

use crate::theme::{self, ThemeName, ThemeSetting};

/// Handle `/theme [name]`.
///
/// - No argument: list all themes with the active one highlighted.
/// - With argument: set the specified theme.
pub(crate) fn handle_theme_command(name: &str) {
    println!("{}", handle_theme_command_str(name));
}

pub(crate) fn handle_theme_command_str(name: &str) -> String {
    if name.is_empty() {
        return list_themes_output();
    }

    match apply_theme(name) {
        Ok(message) | Err(message) => message,
    }
}

pub(crate) fn apply_theme(name: &str) -> Result<String, String> {
    let lower = name.to_lowercase();
    if lower == "auto" {
        let resolved = ThemeSetting::Auto.resolve();
        theme::set_theme(resolved);
        persist_theme_setting(ThemeSetting::Auto);
        return Ok(format!(
            "Theme set to \x1b[1mAuto\x1b[0m (resolved: {})",
            resolved.display_name()
        ));
    }

    let theme_name = lower.parse::<ThemeName>().map_err(|_| {
        format!(
            "\x1b[31mUnknown theme: {}\x1b[0m\n\x1b[2mAvailable: dark, light, dark-daltonized, light-daltonized, dark-ansi, light-ansi, auto\x1b[0m",
            name
        )
    })?;

    theme::set_theme(theme_name);
    persist_theme_setting(name_to_setting(theme_name));
    Ok(format!(
        "Theme set to \x1b[1m{}\x1b[0m",
        theme_name.display_name()
    ))
}

fn list_themes_output() -> String {
    let current = theme::current_theme_name();
    let mut out = String::from("\x1b[1mTerminal Themes\x1b[0m\n\n");

    for &setting in ThemeSetting::ALL {
        let display = setting.display_name();
        let marker = if let Some(theme_name) = setting_to_name(setting) {
            if theme_name == current {
                " \x1b[32m← active\x1b[0m"
            } else {
                ""
            }
        } else if ThemeSetting::Auto.resolve() == current {
            " \x1b[32m← active\x1b[0m"
        } else {
            ""
        };
        let _ = writeln!(out, "  {:<24}{}", display, marker);
    }

    out.push_str("\n\x1b[2mUsage: /theme <name>\x1b[0m\n");
    out.push_str(
        "\x1b[2mNames: dark, light, dark-daltonized, light-daltonized, dark-ansi, light-ansi, auto\x1b[0m",
    );
    out
}

pub(crate) fn setting_to_name(setting: ThemeSetting) -> Option<ThemeName> {
    match setting {
        ThemeSetting::Auto => None,
        ThemeSetting::Dark => Some(ThemeName::Dark),
        ThemeSetting::Light => Some(ThemeName::Light),
        ThemeSetting::DarkDaltonized => Some(ThemeName::DarkDaltonized),
        ThemeSetting::LightDaltonized => Some(ThemeName::LightDaltonized),
        ThemeSetting::DarkAnsi => Some(ThemeName::DarkAnsi),
        ThemeSetting::LightAnsi => Some(ThemeName::LightAnsi),
    }
}

fn name_to_setting(name: ThemeName) -> ThemeSetting {
    match name {
        ThemeName::Dark => ThemeSetting::Dark,
        ThemeName::Light => ThemeSetting::Light,
        ThemeName::DarkDaltonized => ThemeSetting::DarkDaltonized,
        ThemeName::LightDaltonized => ThemeSetting::LightDaltonized,
        ThemeName::DarkAnsi => ThemeSetting::DarkAnsi,
        ThemeName::LightAnsi => ThemeSetting::LightAnsi,
    }
}

/// Persist the theme setting to settings.json (best-effort).
pub(crate) fn persist_theme_setting(setting: ThemeSetting) {
    let Some(config_dir) = clawed_core::config::Settings::claude_dir() else {
        return;
    };
    let settings_path = config_dir.join("settings.json");
    let mut settings: serde_json::Value = if settings_path.exists() {
        std::fs::read_to_string(&settings_path)
            .ok()
            .and_then(|content| serde_json::from_str(&content).ok())
            .unwrap_or_else(|| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };
    settings["theme"] = serde_json::to_value(setting).unwrap_or_default();
    if let Ok(json) = serde_json::to_string_pretty(&settings) {
        if let Some(parent) = settings_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&settings_path, json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_setting_to_name_roundtrip() {
        for &theme_name in ThemeName::ALL {
            let setting = name_to_setting(theme_name);
            assert_eq!(setting_to_name(setting), Some(theme_name));
        }
    }

    #[test]
    fn test_auto_setting_resolves_to_none() {
        assert_eq!(setting_to_name(ThemeSetting::Auto), None);
    }

    #[test]
    fn test_handle_theme_unknown_returns_error_message() {
        let output = handle_theme_command_str("does-not-exist");
        assert!(output.contains("Unknown theme"));
    }
}
