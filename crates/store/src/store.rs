use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use plume_core::Error;

use crate::{GsaAccount, RefreshDevice, secrets};

fn default_true() -> bool {
    true
}

/// Light or dark appearance; `System` follows the OS setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppearanceMode {
    #[default]
    System,
    Light,
    Dark,
}

impl AppearanceMode {
    pub const ALL: [AppearanceMode; 3] = [
        AppearanceMode::System,
        AppearanceMode::Light,
        AppearanceMode::Dark,
    ];
}

/// A device that installs are tied to while device locking is enabled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockedDevice {
    pub udid: String,
    pub name: String,
}

/// Joining a tailnet from inside the app to reach devices over Tailscale.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TailscaleSettings {
    #[serde(default)]
    pub enabled: bool,
    /// The machine name requested for this computer in the tailnet.
    #[serde(default = "default_tailscale_hostname")]
    pub hostname: String,
    /// A custom coordination server (e.g. Headscale); `None` uses Tailscale's.
    #[serde(default)]
    pub control_url: Option<String>,
    /// Device machine names or tailnet IP addresses to look for.
    #[serde(default)]
    pub devices: Vec<String>,
}

fn default_tailscale_hostname() -> String {
    "jimpactor".into()
}

impl Default for TailscaleSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            hostname: default_tailscale_hostname(),
            control_url: None,
            devices: Vec::new(),
        }
    }
}

/// Where the JIT server accepts connections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JitListen {
    /// Only this computer.
    Local,
    /// Every network interface, so devices on the LAN can reach it.
    #[default]
    Network,
    /// This computer's tailnet address.
    Tailnet,
}

impl JitListen {
    pub const ALL: [JitListen; 3] = [JitListen::Local, JitListen::Network, JitListen::Tailnet];
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JitServerSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_jit_port")]
    pub port: u16,
    #[serde(default)]
    pub listen: JitListen,
}

fn default_jit_port() -> u16 {
    9172
}

impl Default for JitServerSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            port: default_jit_port(),
            listen: JitListen::default(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AccountStore {
    selected_account: Option<String>,      // Email
    accounts: HashMap<String, GsaAccount>, // Email -> GsaAccount
    #[serde(default)]
    refreshes: HashMap<String, RefreshDevice>, // UDID -> RefreshDevice (apps?)
    #[serde(default)]
    locale: Option<String>, // None = system locale
    #[serde(default = "default_true")]
    use_keyring: bool, // Keep account tokens in the OS keyring instead of this file
    #[serde(default)]
    remote_pairing: bool, // Include a remote pairing (RPPairing) record in pairing files
    #[serde(default)]
    appearance: AppearanceMode,
    #[serde(default)]
    lock_device: bool, // Installs only go to the device chosen with Select
    #[serde(default)]
    locked_device: Option<LockedDevice>,
    #[serde(default = "default_true")]
    close_to_tray: bool, // Closing the window keeps the app running in the tray
    #[serde(default)]
    tailscale: TailscaleSettings,
    #[serde(default)]
    jit_server: JitServerSettings,
    #[serde(skip)]
    path: Option<PathBuf>,
    #[serde(skip)]
    secrets_on_disk: bool, // Loaded file still holds tokens that belong in the keyring
}

impl Default for AccountStore {
    fn default() -> Self {
        Self {
            selected_account: None,
            accounts: HashMap::new(),
            refreshes: HashMap::new(),
            locale: None,
            use_keyring: true,
            remote_pairing: false,
            appearance: AppearanceMode::default(),
            lock_device: false,
            locked_device: None,
            close_to_tray: true,
            tailscale: TailscaleSettings::default(),
            jit_server: JitServerSettings::default(),
            path: None,
            secrets_on_disk: false,
        }
    }
}

impl AccountStore {
    pub async fn load(path: &Option<PathBuf>) -> Result<Self, Error> {
        if let Some(path) = path {
            let mut settings = if !path.exists() {
                Self::default()
            } else {
                let contents = tokio::fs::read_to_string(path).await?;
                Self::parse(&contents)?
            };
            settings.path = Some(path.clone());
            Ok(settings)
        } else {
            Ok(Self::default())
        }
    }

    pub fn load_sync(path: &Option<PathBuf>) -> Result<Self, Error> {
        if let Some(path) = path {
            let mut settings = if !path.exists() {
                Self::default()
            } else {
                let contents = std::fs::read_to_string(path)?;
                Self::parse(&contents)?
            };
            settings.path = Some(path.clone());
            Ok(settings)
        } else {
            Ok(Self::default())
        }
    }

    pub async fn save(&self) -> Result<(), Error> {
        if let Some(path) = &self.path {
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }

            tokio::fs::write(path, self.to_json()?).await?;
        }
        Ok(())
    }

    pub fn save_sync(&self) -> Result<(), Error> {
        if let Some(path) = &self.path {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }

            std::fs::write(path, self.to_json()?)?;
        }
        Ok(())
    }

    fn parse(contents: &str) -> Result<Self, Error> {
        let mut store: Self = serde_json::from_str(contents)?;
        for (email, account) in &mut store.accounts {
            if !account.xcode_gs_token().is_empty() {
                store.secrets_on_disk = true;
            } else if let Some(token) = secrets::load_token(email) {
                account.set_xcode_gs_token(token);
            }
        }
        Ok(store)
    }

    /// Serializes the store, moving tokens into the OS keyring when enabled.
    fn to_json(&self) -> Result<String, Error> {
        let mut on_disk = self.clone();
        if secrets::keyring_available() {
            for (email, account) in &mut on_disk.accounts {
                if !self.use_keyring {
                    secrets::delete_token(email);
                    continue;
                }
                let token = account.xcode_gs_token();
                let stored = secrets::load_token(email).as_ref() == Some(token)
                    || secrets::store_token(email, token);
                if stored {
                    account.set_xcode_gs_token(String::new());
                }
            }
        }
        Ok(serde_json::to_string_pretty(&on_disk)?)
    }

    /// Rewrites the file if it still holds tokens that should live in the keyring.
    pub fn migrate_secrets_sync(&mut self) -> Result<(), Error> {
        if self.secrets_on_disk && self.use_keyring && secrets::keyring_available() {
            self.save_sync()?;
            self.secrets_on_disk = false;
        }
        Ok(())
    }

    pub fn use_keyring(&self) -> bool {
        self.use_keyring
    }

    pub fn set_use_keyring_sync(&mut self, enabled: bool) -> Result<(), Error> {
        self.use_keyring = enabled;
        self.save_sync()
    }

    pub fn remote_pairing(&self) -> bool {
        self.remote_pairing
    }

    pub fn set_remote_pairing_sync(&mut self, enabled: bool) -> Result<(), Error> {
        self.remote_pairing = enabled;
        self.save_sync()
    }

    pub fn appearance(&self) -> AppearanceMode {
        self.appearance
    }

    pub fn set_appearance_sync(&mut self, appearance: AppearanceMode) -> Result<(), Error> {
        self.appearance = appearance;
        self.save_sync()
    }

    pub fn lock_device(&self) -> bool {
        self.lock_device
    }

    pub fn set_lock_device_sync(&mut self, enabled: bool) -> Result<(), Error> {
        self.lock_device = enabled;
        self.save_sync()
    }

    pub fn locked_device(&self) -> Option<&LockedDevice> {
        self.locked_device.as_ref()
    }

    pub fn set_locked_device_sync(&mut self, device: Option<LockedDevice>) -> Result<(), Error> {
        self.locked_device = device;
        self.save_sync()
    }

    pub fn close_to_tray(&self) -> bool {
        self.close_to_tray
    }

    pub fn set_close_to_tray_sync(&mut self, enabled: bool) -> Result<(), Error> {
        self.close_to_tray = enabled;
        self.save_sync()
    }

    pub fn tailscale(&self) -> &TailscaleSettings {
        &self.tailscale
    }

    pub fn set_tailscale_sync(&mut self, settings: TailscaleSettings) -> Result<(), Error> {
        self.tailscale = settings;
        self.save_sync()
    }

    pub fn jit_server(&self) -> &JitServerSettings {
        &self.jit_server
    }

    pub fn set_jit_server_sync(&mut self, settings: JitServerSettings) -> Result<(), Error> {
        self.jit_server = settings;
        self.save_sync()
    }

    pub fn accounts(&self) -> &HashMap<String, GsaAccount> {
        &self.accounts
    }

    pub fn path(&self) -> Option<PathBuf> {
        self.path.clone()
    }

    pub fn get_account(&self, email: &str) -> Option<&GsaAccount> {
        self.accounts.get(email)
    }

    pub async fn accounts_add(&mut self, account: GsaAccount) -> Result<(), Error> {
        let email = account.email().clone();
        self.accounts.insert(email.clone(), account);
        self.selected_account = Some(email);
        self.save().await
    }

    pub fn accounts_add_sync(&mut self, account: GsaAccount) -> Result<(), Error> {
        let email = account.email().clone();
        self.accounts.insert(email.clone(), account);
        self.selected_account = Some(email);
        self.save_sync()
    }

    pub async fn accounts_remove(&mut self, email: &str) -> Result<(), Error> {
        self.accounts.remove(email);
        secrets::delete_token(email);
        if self.selected_account.as_ref() == Some(&email.to_string()) {
            self.selected_account = None;
        }
        self.save().await
    }

    pub fn accounts_remove_sync(&mut self, email: &str) -> Result<(), Error> {
        self.accounts.remove(email);
        secrets::delete_token(email);
        if self.selected_account.as_ref() == Some(&email.to_string()) {
            self.selected_account = None;
        }
        self.save_sync()
    }

    pub async fn account_select(&mut self, email: &str) -> Result<(), Error> {
        if self.accounts.contains_key(email) {
            self.selected_account = Some(email.to_string());
            self.save().await
        } else {
            Err(Error::Parse) // we need better errors
        }
    }

    pub fn account_select_sync(&mut self, email: &str) -> Result<(), Error> {
        if self.accounts.contains_key(email) {
            self.selected_account = Some(email.to_string());
            self.save_sync()
        } else {
            Err(Error::Parse) // we need better errors
        }
    }

    pub fn selected_account(&self) -> Option<&GsaAccount> {
        if let Some(email) = &self.selected_account {
            self.accounts.get(email)
        } else {
            None
        }
    }

    pub fn locale(&self) -> Option<&str> {
        self.locale.as_deref()
    }

    pub fn set_locale_sync(&mut self, locale: Option<String>) -> Result<(), Error> {
        self.locale = locale;
        self.save_sync()
    }

    pub async fn accounts_add_from_session(
        &mut self,
        email: String,
        account: plume_core::auth::Account,
    ) -> Result<(), Error> {
        let first_name = account.get_name().0;
        let s = plume_core::developer::DeveloperSession::using_account(account).await?;
        let teams_response = s.qh_list_teams().await?;
        let adsid = s.adsid().clone();
        let xcode_gs_token = s.xcode_gs_token().clone();

        let team_id = if teams_response.teams.len() != 1 {
            "".to_string()
        } else {
            teams_response.teams[0].team_id.clone()
        };

        let account = GsaAccount::new(email, first_name, adsid, xcode_gs_token, team_id);

        self.accounts_add(account).await?;

        Ok(())
    }

    pub async fn update_account_team(&mut self, email: &str, team_id: String) -> Result<(), Error> {
        if let Some(account) = self.accounts.get_mut(email) {
            account.set_team_id(team_id);
            self.save().await
        } else {
            Err(Error::Parse)
        }
    }

    pub fn update_account_team_sync(&mut self, email: &str, team_id: String) -> Result<(), Error> {
        if let Some(account) = self.accounts.get_mut(email) {
            account.set_team_id(team_id);
            self.save_sync()
        } else {
            Err(Error::Parse)
        }
    }

    pub fn refreshes(&self) -> &HashMap<String, RefreshDevice> {
        &self.refreshes
    }

    pub fn get_refresh_device(&self, udid: &str) -> Option<&RefreshDevice> {
        self.refreshes.get(udid)
    }

    pub async fn add_or_update_refresh_device(
        &mut self,
        device: RefreshDevice,
    ) -> Result<(), Error> {
        self.refreshes.insert(device.udid.clone(), device);
        self.save().await
    }

    pub fn add_or_update_refresh_device_sync(
        &mut self,
        device: RefreshDevice,
    ) -> Result<(), Error> {
        self.refreshes.insert(device.udid.clone(), device);
        self.save_sync()
    }

    pub async fn remove_refresh_device(&mut self, udid: &str) -> Result<(), Error> {
        self.refreshes.remove(udid);
        self.save().await
    }

    pub fn remove_refresh_device_sync(&mut self, udid: &str) -> Result<(), Error> {
        self.refreshes.remove(udid);
        self.save_sync()
    }
}
