//! Account secrets kept in the OS credential store (Keychain, Credential Manager, Secret Service).

use std::sync::OnceLock;

const SERVICE: &str = "Jimpactor";

fn entry(email: &str) -> Option<keyring::Entry> {
    keyring::Entry::new(SERVICE, email)
        .inspect_err(|e| log::warn!("Failed to open keyring entry: {e}"))
        .ok()
}

/// Whether the OS credential store works on this machine, checked once per process.
pub fn keyring_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        let Ok(entry) = keyring::Entry::new(SERVICE, "availability_check") else {
            return false;
        };
        let works = entry.set_password("ok").is_ok() && entry.get_password().is_ok();
        let _ = entry.delete_credential();
        if !works {
            log::warn!("OS keyring is unavailable; credentials will be stored on disk");
        }
        works
    })
}

pub(crate) fn store_token(email: &str, token: &str) -> bool {
    entry(email)
        .map(|e| {
            e.set_password(token)
                .inspect_err(|err| log::warn!("Failed to store token in keyring: {err}"))
                .is_ok()
        })
        .unwrap_or(false)
}

pub(crate) fn load_token(email: &str) -> Option<String> {
    match entry(email)?.get_password() {
        Ok(token) => Some(token),
        Err(keyring::Error::NoEntry) => None,
        Err(err) => {
            log::warn!("Failed to read token from keyring: {err}");
            None
        }
    }
}

pub(crate) fn delete_token(email: &str) {
    if let Some(entry) = entry(email) {
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => {}
            Err(err) => log::warn!("Failed to delete token from keyring: {err}"),
        }
    }
}
