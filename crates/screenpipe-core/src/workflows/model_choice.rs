// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com

//! One persisted choice for interactive and scheduled Workflows AI.
use crate::pipes::{read_store_bin, resolve_exact_preset, ResolvedPreset};
use anyhow::{bail, Context, Result};
use std::path::Path;

pub const PRIVATE_MODEL: &str = "glm-5.3-flash-reap50-iq3m";
pub fn selected_provider() -> Result<ResolvedPreset> {
    read_provider(&crate::paths::default_screenpipe_data_dir())
}
fn read_provider(base: &Path) -> Result<ResolvedPreset> {
    let text = match std::fs::read_to_string(base.join("workflows-model.json")) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            "{\"mode\":\"default\"}".into()
        }
        Err(error) => return Err(error.into()),
    };
    let value: serde_json::Value = serde_json::from_str(&text)?;
    let preset = match value["mode"].as_str() {
        // Retired hosted modes inherit the user's configured provider. Never
        // recreate a cloud preset or require a Screenpipe account session.
        Some("default" | "intelligent" | "private") => {
            let store = read_store_bin(&base.join("store.bin"))
                .context("Choose a local model or your own AI provider in Settings.")?;
            let presets = store["settings"]["aiPresets"]
                .as_array()
                .context("No AI provider is configured. Choose one in Settings.")?;
            let compatible =
                |preset: &&serde_json::Value| preset["provider"].as_str() != Some("acp");
            let selected = presets
                .iter()
                .filter(compatible)
                .find(|preset| preset["defaultPreset"].as_bool() == Some(true))
                .or_else(|| {
                    presets
                        .iter()
                        .filter(compatible)
                        .find(|preset| preset["id"].as_str() == Some("pipes"))
                })
                .or_else(|| presets.iter().find(|preset| compatible(preset)))
                .and_then(|preset| preset["id"].as_str())
                .context("No compatible AI provider is configured for Workflows.")?;
            resolve_exact_preset(&base.join("pipes"), selected)
                .context("Your configured Workflows AI provider is unavailable.")?
        }
        Some(mode) if mode.starts_with("preset:") && !mode[7..].trim().is_empty() => {
            resolve_exact_preset(&base.join("pipes"), &mode[7..])
                .context("Your Workflows AI preset is unavailable. Choose an AI preset again.")?
        }
        _ => bail!("The saved Workflows AI choice is invalid. Choose an AI preset again."),
    };
    let retired_gateway = preset
        .url
        .as_deref()
        .and_then(|url| reqwest::Url::parse(url).ok())
        .and_then(|url| url.host_str().map(str::to_lowercase))
        .is_some_and(|host| {
            ["screenpipe.com", "screenpi.pe"]
                .iter()
                .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
        });
    if preset.model.trim().is_empty()
        || retired_gateway
        || !matches!(
            preset.provider.as_deref(),
            Some("openai" | "openai-chatgpt" | "anthropic" | "custom" | "ollama")
        )
    {
        bail!("Choose a local model or your own AI provider for Workflows in Settings.");
    }
    Ok(preset)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_private_selection_and_rejects_corrupt_preferences() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workflows-model.json");
        assert_eq!(read_provider(dir.path()).unwrap().model, "auto");
        std::fs::write(&path, r#"{"mode":"private"}"#).unwrap();
        assert_eq!(read_provider(dir.path()).unwrap().model, PRIVATE_MODEL);
        for invalid in ["{", r#"{"mode":"custom"}"#, r#"{"mode":"preset:"}"#, "null"] {
            std::fs::write(&path, invalid).unwrap();
            assert!(read_provider(dir.path()).is_err());
        }
    }
    #[test]
    fn resolves_exact_current_preset_and_never_falls_back_to_cloud() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("workflows-model.json"),
            r#"{"mode":"preset:default"}"#,
        )
        .unwrap();
        let store = dir.path().join("store.bin");
        std::fs::write(&store, r#"{"settings":{"aiPresets":[
            {"id":"chat","provider":"screenpipe-cloud","model":"auto","defaultPreset":true},
            {"id":"default","provider":"custom","model":"my-model","url":"http://localhost:1234/v1","apiKey":"fixture-key"}
        ]}}"#).unwrap();
        let preset = read_provider(dir.path()).unwrap();
        assert_eq!(preset.model, "my-model");
        assert_eq!(preset.provider.as_deref(), Some("custom"));
        assert_eq!(preset.url.as_deref(), Some("http://localhost:1234/v1"));
        assert_eq!(preset.api_key.as_deref(), Some("fixture-key"));
        std::fs::write(&store, r#"{"settings":{"aiPresets":[]}}"#).unwrap();
        assert!(read_provider(dir.path()).is_err());
        std::fs::remove_file(&store).unwrap();
        std::fs::write(
            dir.path().join("workflows-model.json"),
            r#"{"mode":"preset:chat"}"#,
        )
        .unwrap();
        assert!(read_provider(dir.path()).is_err());
        assert!(
            !store.exists(),
            "exact selection must not recreate cloud presets"
        );
    }
}
