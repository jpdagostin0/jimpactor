use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::sideloadly_auth::SideloadlySession;
use plume_core::remote_servers::AnisetteServer;
use plume_core::{
    ANISETTE_STATE_FILE, AnisetteConfiguration, AnisetteProvider, Error, FAKE_IDENTITY_DIR,
};

pub const ANISETTE_SETTINGS_FILE: &str = "anisette.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnisetteMode {
    Local,
    #[default]
    Remote,
    Fake,
}

#[cfg(test)]
mod sideloadly_tests {
    use super::*;
    use crate::SideloadlyAccount;

    #[test]
    fn remote_reset_clears_current_state_and_preserves_accounts_and_legacy_data() {
        let path = std::env::temp_dir().join(format!("jimpactor-reset-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&path).unwrap();
        for file in [
            ANISETTE_STATE_FILE,
            "state.plist",
            "accounts.json",
            "certificate.p12",
        ] {
            std::fs::write(path.join(file), "synthetic").unwrap();
        }
        AnisetteSettings::default().reset_identity(&path).unwrap();
        assert!(!path.join(ANISETTE_STATE_FILE).exists());
        for file in ["state.plist", "accounts.json", "certificate.p12"] {
            assert_eq!(
                std::fs::read_to_string(path.join(file)).unwrap(),
                "synthetic"
            );
        }
        AnisetteSettings::default().reset_identity(&path).unwrap();
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn old_settings_load_without_a_sideloadly_session() {
        let settings: AnisetteSettings = serde_json::from_str(r#"{"mode":"remote"}"#).unwrap();
        assert!(settings.sideloadly_session.is_none());
    }

    #[test]
    fn logout_clears_sideloadly_and_preserves_other_provider_settings() {
        let mut settings = AnisetteSettings::default();
        let custom = AnisetteServer::new("custom", "https://ani.example.com");
        let session = SideloadlySession {
            account: SideloadlyAccount::Anonymous,
            server: AnisetteServer::new(
                "Sideloadly",
                "https://sideloadly.io/anisette/example?u=example",
            ),
        };
        settings.remote_server = custom.clone();
        settings.sideloadly_session = Some(session.clone());
        assert_eq!(settings.logout_sideloadly(), Some(session.clone()));
        assert_eq!(settings.remote_server, custom);

        settings.remote_provider = RemoteProvider::Sideloadly;
        settings.remote_server = session.server.clone();
        settings.sideloadly_session = Some(session);
        settings.logout_sideloadly();
        assert!(settings.sideloadly_session.is_none());
        assert_eq!(settings.remote_provider, RemoteProvider::SideStore);
        assert_eq!(settings.remote_server, default_remote_server());
    }
}

impl AnisetteMode {
    pub const ALL: [AnisetteMode; 3] = [
        AnisetteMode::Fake,
        AnisetteMode::Local,
        AnisetteMode::Remote,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RemoteProvider {
    #[default]
    SideStore,
    Sideloadly,
    Custom,
}

impl RemoteProvider {
    pub const ALL: [RemoteProvider; 3] = [
        RemoteProvider::Sideloadly,
        RemoteProvider::SideStore,
        RemoteProvider::Custom,
    ];
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
    #[serde(default)]
    pub remote_provider: RemoteProvider,
    #[serde(default)]
    pub sideloadly_session: Option<SideloadlySession>,
}

fn default_remote_server() -> AnisetteServer {
    plume_core::remote_servers::builtin_servers().remove(0)
}

impl Default for AnisetteSettings {
    fn default() -> Self {
        Self {
            mode: AnisetteMode::default(),
            remote_server: default_remote_server(),
            remote_provider: RemoteProvider::default(),
            sideloadly_session: None,
        }
    }
}

impl AnisetteSettings {
    /// Removes our Sideloadly connection while preserving an active SideStore server.
    pub fn logout_sideloadly(&mut self) -> Option<SideloadlySession> {
        let previous = self.sideloadly_session.take();
        if self.remote_provider == RemoteProvider::Sideloadly {
            self.remote_provider = RemoteProvider::SideStore;
            self.remote_server = default_remote_server();
        }
        previous
    }

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
            AnisetteMode::Remote => remove_file(data_path.join(ANISETTE_STATE_FILE))?,
            AnisetteMode::Local => {
                remove_file(data_path.join("adi.pb"))?;
                remove_file(data_path.join("identifier"))?;
            }
            AnisetteMode::Fake => {
                match std::fs::remove_dir_all(data_path.join(FAKE_IDENTITY_DIR)) {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                    _ => {}
                }
            }
        }
        Ok(())
    }
}
