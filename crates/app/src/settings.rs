//! What the app remembers about the classifier between launches.
//!
//! Three fields, none of them secret. The key is in Credential Manager
//! (`keys.rs`); this file only says which provider and model it belongs to, and
//! whether image posts may be sent as images.
//!
//! Reading tolerates a missing or corrupt file rather than failing: the worst
//! case is a fresh default, and refusing to start because a settings file was
//! hand-edited would be a worse failure than the one it reports.

use std::path::Path;

use serde::{Deserialize, Serialize};
use xdl_core::inference::ProviderConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub provider: ProviderConfig,
    /// Image posts are sent as images. Off keeps everything text-only, which is
    /// what a model without vision needs.
    pub send_images: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { provider: ProviderConfig::default_config(), send_images: true }
    }
}

pub fn load(path: &Path) -> Settings {
    let mut settings: Settings = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    // A file written before the field existed has no pace in it; fill it from
    // the preset rather than leaving a one-a-minute account at 60.
    settings.provider.resolve_pace();
    settings
}

pub fn save(path: &Path, settings: &Settings) -> Result<(), String> {
    let json = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| format!("could not save settings: {e}"))
}
