use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Fixture data and simulated commands; file edits are still real.
    Mock,
    /// Talks to the installed nix and home-manager.
    Real,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub mode: Mode,
    /// The Nix file the GUI edits.
    pub config_file: String,
    /// Flake output to build, e.g. `~/.config/home-manager#adham`. When empty,
    /// home-manager is run in classic mode with `-f config_file`.
    pub flake_ref: String,
    /// Explicit path to home-manager's options.json. When empty, it is built
    /// from `home_manager_flake`.
    pub options_json: String,
    pub home_manager_flake: String,
}

impl Settings {
    pub fn default_for(mock_file: &Path) -> Self {
        let real = crate::nix::nix_available();
        Settings {
            mode: if real { Mode::Real } else { Mode::Mock },
            config_file: if real {
                default_real_config_file()
            } else {
                mock_file.display().to_string()
            },
            flake_ref: String::new(),
            options_json: String::new(),
            home_manager_flake: "github:nix-community/home-manager".into(),
        }
    }

    pub fn load(path: &Path, mock_file: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| Self::default_for(mock_file))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)
            .with_context(|| format!("couldn't write {}", path.display()))
    }

    pub fn config_path(&self) -> PathBuf {
        expand_home(&self.config_file)
    }
}

pub fn default_real_config_file() -> String {
    "~/.config/home-manager/home.nix".into()
}

pub fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(path),
    }
}
