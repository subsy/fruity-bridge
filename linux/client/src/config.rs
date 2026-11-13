use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// `~/.config/fruity-bridge/config.toml`, shared by both apps.
/// The `FRUITY_BRIDGE_URL` environment variable overrides `url` (used for the mock server).
/// There's no default address: the apps ask for it on first run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    pub url: String,
}

impl Config {
    /// Whether an address has been set (saved or from the environment).
    pub fn is_set(&self) -> bool {
        !self.url.trim().is_empty()
    }

    pub fn path() -> Option<PathBuf> {
        Some(dirs::config_dir()?.join("fruity-bridge").join("config.toml"))
    }

    pub fn load() -> Config {
        let mut config: Config = Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default();
        if let Ok(url) = std::env::var("FRUITY_BRIDGE_URL")
            && !url.is_empty()
        {
            config.url = url;
        }
        config
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::path().ok_or_else(|| std::io::Error::other("no config directory"))?;
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(path, toml::to_string(self).map_err(std::io::Error::other)?)
    }
}
