use std::path::PathBuf;

mod anisette;
mod gsa_account;
mod refresh;
mod secrets;
mod sideloadly_auth;
mod sideloadly_import;
mod store;
/// The folder holding accounts, settings and caches, created if needed. Every front end
/// uses the same one so they see the same accounts.
pub fn data_path() -> PathBuf {
    let base = if cfg!(windows) {
        std::env::var("APPDATA").unwrap_or_else(|_| ".".into())
    } else {
        std::env::var("XDG_CONFIG_HOME")
            .unwrap_or_else(|_| std::env::var("HOME").unwrap_or_else(|_| ".".into()) + "/.config")
    };
    let dir = PathBuf::from(base).join("Jimpactor");
    std::fs::create_dir_all(&dir).ok();
    dir
}

pub use anisette::{ANISETTE_SETTINGS_FILE, AnisetteMode, AnisetteSettings, RemoteProvider};
pub use gsa_account::{GsaAccount, account_from_session};
pub use refresh::{RefreshApp, RefreshDevice};
pub use secrets::keyring_available;
pub use sideloadly_auth::{
    DEFAULT_CLIENT_ID, SideloadlyAccount, SideloadlyAuth, SideloadlySession,
};
pub use sideloadly_import::{load_sideloadly_servers, parse_sessions, sessions_paths};
pub use store::{
    AccountStore, AppearanceMode, AscKey, JitListen, JitPassword, JitServerSettings, LockedDevice,
    TailscaleSettings,
};
