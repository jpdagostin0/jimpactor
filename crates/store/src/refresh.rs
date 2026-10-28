use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RefreshDevice {
    pub udid: String,          // Device UDID
    pub name: String,          // Device name
    pub account: String,       // Email
    pub apps: Vec<RefreshApp>, // Device apps to refresh
    pub is_mac: bool,          // m1 sideloading
}

impl RefreshDevice {
    /// Called only after installation succeeds, so a failed attempt cannot change refresh ownership.
    pub fn record_install(&mut self, account: String, app: RefreshApp) {
        self.account = account;
        if let Some(existing) = self
            .apps
            .iter_mut()
            .find(|existing| existing.bundle_id == app.bundle_id)
        {
            *existing = app;
        } else {
            self.apps.push(app);
        }
    }
}

// custom entitlements not supported
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RefreshApp {
    pub path: PathBuf,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub bundle_id: Option<String>,
    pub scheduled_refresh: DateTime<Utc>, // the scheduled refresh time will happen a day before expiration
}

// to support autorefreshing of apps we need to store a modified copy of the app first
// MISAGENT:
//   for this, we can just reregister the bundle and collect the provisioning profiles,
//
// MANUAL (CERTIFICATE REVOKED):
//   we have a modified copy of the app already, we can just resign and register the bundle and attempt to install it

// TODO: replace substrate with ellekit
// TODO: maybe some 26.0 macho fixes.

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reinstall_replaces_refresh_account_and_app_without_duplicates() {
        let app = RefreshApp {
            path: "old.ipa".into(),
            name: Some("App".into()),
            bundle_id: Some("com.example.app".into()),
            scheduled_refresh: Utc::now(),
        };
        let mut device = RefreshDevice {
            udid: "test-device".into(),
            name: "Test".into(),
            account: "previous@example.com".into(),
            apps: vec![app.clone()],
            is_mac: false,
        };
        let mut replacement = app;
        replacement.path = "new.ipa".into();
        device.record_install("current@example.com".into(), replacement);
        let persisted: RefreshDevice =
            serde_json::from_str(&serde_json::to_string(&device).unwrap()).unwrap();
        assert_eq!(persisted.account, "current@example.com");
        assert_eq!(persisted.apps.len(), 1);
        assert_eq!(persisted.apps[0].path, PathBuf::from("new.ipa"));
    }
}
