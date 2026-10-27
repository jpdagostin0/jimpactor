use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use plume_core::remote_servers::AnisetteServer;
use plume_core::{AnisetteConfiguration, AnisetteProvider, Error, FAKE_IDENTITY_DIR};

pub const ANISETTE_SETTINGS_FILE: &str = "anisette.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnisetteMode {
    Local,
    #[default]
    Remote,
    Fake,
}

impl AnisetteMode {
    pub const ALL: [AnisetteMode; 3] = [AnisetteMode::Local, AnisetteMode::Remote, AnisetteMode::Fake];
}

impl From<AnisetteMode> for AnisetteProvider {
    fn from(mode: AnisetteMode) -> Self {
        match mode {
            AnisetteMode::Local => AnisetteProvider::Local,
            AnisetteMode::Remote => AnisetteProvider::Remote,
            AnisetteMode::Fake => AnisetteProvider::Fake,
        }
    }
}

/// Which anisette provider to use, stored in `anisette.json` next to `accounts.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnisetteSettings {
    #[serde(default)]
    pub mode: AnisetteMode,
    #[serde(default = "default_remote_server")]
    pub remote_server: AnisetteServer,
    /// Sideloadly instance host for the login flow; empty uses the default instance.
    #[serde(default)]
    pub sideloadly_host: String,
    /// OAuth client id for the Sideloadly login; empty uses Impactor's.
    #[serde(default)]
    pub sideloadly_client_id: String,
}

fn default_remote_server() -> AnisetteServer {
    plume_core::remote_servers::builtin_servers().remove(0)
}

impl Default for AnisetteSettings {
    fn default() -> Self {
        Self {
            mode: AnisetteMode::default(),
            remote_server: default_remote_server(),
            sideloadly_host: String::new(),
            sideloadly_client_id: String::new(),
        }
    }
}

impl AnisetteSettings {
    pub fn path(data_path: &Path) -> PathBuf {
        data_path.join(ANISETTE_SETTINGS_FILE)
    }

    /// Loads the settings, falling back to defaults if the file is missing or invalid.
    pub fn load_sync(data_path: &Path) -> Self {
        std::fs::read_to_string(Self::path(data_path))
            .ok()
            .and_then(|contents| serde_json::from_str(&contents).ok())
            .unwrap_or_default()
    }

    pub fn save_sync(&self, data_path: &Path) -> Result<(), Error> {
        std::fs::create_dir_all(data_path)?;
        std::fs::write(Self::path(data_path), serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn configuration(&self, data_path: &Path) -> AnisetteConfiguration {
        AnisetteConfiguration::default()
            .set_configuration_path(data_path.to_path_buf())
            .set_provider(self.mode.into())
            .set_anisette_url_v3(self.remote_server.address.clone())
    }

    /// Anisette configuration for the settings saved in `data_path`.
    pub fn load_configuration(data_path: &Path) -> AnisetteConfiguration {
        Self::load_sync(data_path).configuration(data_path)
    }

    /// Forgets the provisioned identity of the current mode, so a new one is created on next use.
    pub fn reset_identity(&self, data_path: &Path) -> Result<(), Error> {
        let remove_file = |path: PathBuf| match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
        match self.mode {
            AnisetteMode::Remote => remove_file(data_path.join("state.plist"))?,
            AnisetteMode::Local => {
                remove_file(data_path.join("adi.pb"))?;
                remove_file(data_path.join("identifier"))?;
            }
            AnisetteMode::Fake => match std::fs::remove_dir_all(data_path.join(FAKE_IDENTITY_DIR)) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                _ => {}
            },
        }
        Ok(())
    }
}
