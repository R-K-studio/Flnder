use std::{fs, path::Path};

use anyhow::Result;

use crate::models::{AppSettings, SaveSettingsInput};

pub fn load_settings(path: &Path) -> Result<AppSettings> {
    if !path.exists() {
        let defaults = AppSettings::defaults();
        save_settings(path, &defaults)?;
        return Ok(defaults);
    }

    let raw = fs::read_to_string(path)?;
    let settings = serde_json::from_str::<AppSettings>(&raw).unwrap_or_else(|_| AppSettings::defaults());
    Ok(settings)
}

pub fn save_settings(path: &Path, settings: &AppSettings) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_string_pretty(settings)?;
    fs::write(path, body)?;
    Ok(())
}

pub fn merge_settings(existing: &AppSettings, input: SaveSettingsInput) -> AppSettings {
    AppSettings {
        api_base: strip_wrapping_quotes(&input.api_base),
        api_key: input.api_key,
        vision_model: strip_wrapping_quotes(&input.vision_model),
        fast_answer_model: strip_wrapping_quotes(&input.fast_answer_model),
        answer_model: strip_wrapping_quotes(&input.answer_model),
        embedding_model: strip_wrapping_quotes(&input.embedding_model),
        output_dir: strip_wrapping_quotes(&input.output_dir),
        shortcut: strip_wrapping_quotes(&input.shortcut),
        text_shortcut: strip_wrapping_quotes(&input.text_shortcut),
        active_course_id: input.active_course_id.or_else(|| existing.active_course_id.clone()),
    }
}

fn strip_wrapping_quotes(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() >= 2 {
        let bytes = trimmed.as_bytes();
        let first = bytes[0] as char;
        let last = bytes[trimmed.len() - 1] as char;
        if (first == '\'' && last == '\'') || (first == '"' && last == '"') {
            return trimmed[1..trimmed.len() - 1].trim().to_string();
        }
    }
    trimmed.to_string()
}
