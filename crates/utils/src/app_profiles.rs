//! Saved install options for an app, reused the next time it is signed or updated.
//! One JSON file per bundle identifier, under `app_profiles` in the data folder.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Error, SignerOptions};

const FOLDER: &str = "app_profiles";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppProfile {
    /// The bundle identifier in the original IPA, before any override.
    pub bundle_id: String,
    pub options: SignerOptions,
}

impl AppProfile {
    /// Replaces `options` with this profile, keeping the app type detected from the IPA.
    /// Returns files the profile points at that no longer exist; those are left out.
    pub fn apply_to(&self, options: &mut SignerOptions) -> Vec<PathBuf> {
        let app = options.app;
        *options = self.options.clone();
        options.app = app;

        let mut missing = Vec::new();
        let mut keep = |path: &PathBuf| {
            let exists = path.exists();
            if !exists {
                missing.push(path.clone());
            }
            exists
        };
        options.custom_icon = options.custom_icon.take().filter(&mut keep);
        options.custom_entitlements = options.custom_entitlements.take().filter(&mut keep);
        if let Some(tweaks) = &mut options.tweaks {
            tweaks.retain(&mut keep);
        }
        missing
    }

    pub fn read(path: &Path) -> Result<Self, Error> {
        serde_json::from_slice(&std::fs::read(path)?)
            .map_err(|e| Error::Other(format!("{} is not an app profile: {e}", path.display())))
    }

    pub fn write(&self, path: &Path) -> Result<(), Error> {
        let json = serde_json::to_vec_pretty(self).map_err(|e| Error::Other(e.to_string()))?;
        Ok(std::fs::write(path, json)?)
    }
}

/// The saved profiles in a data folder.
pub struct AppProfiles {
    folder: PathBuf,
}

impl AppProfiles {
    pub fn new(data_path: &Path) -> Self {
        Self {
            folder: data_path.join(FOLDER),
        }
    }

    /// Bundle identifiers can come from any IPA, so never let one choose a path.
    fn path(&self, bundle_id: &str) -> PathBuf {
        let name: String = bundle_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || "-_.".contains(c) {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        self.folder.join(format!("{name}.json"))
    }

    pub fn load(&self, bundle_id: &str) -> Option<AppProfile> {
        let path = self.path(bundle_id);
        if !path.exists() {
            return None;
        }
        AppProfile::read(&path)
            .inspect_err(|error| log::warn!("Ignoring saved profile: {error}"))
            .ok()
    }

    pub fn save(&self, profile: &AppProfile) -> Result<(), Error> {
        std::fs::create_dir_all(&self.folder)?;
        profile.write(&self.path(&profile.bundle_id))
    }

    pub fn delete(&self, bundle_id: &str) -> Result<(), Error> {
        match std::fs::remove_file(self.path(bundle_id)) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
            _ => Ok(()),
        }
    }

    /// Every readable profile, by bundle identifier.
    pub fn list(&self) -> Vec<AppProfile> {
        let Ok(entries) = std::fs::read_dir(&self.folder) else {
            return Vec::new();
        };
        let mut profiles: Vec<_> = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|e| e == "json"))
            .filter_map(|entry| AppProfile::read(&entry.path()).ok())
            .collect();
        profiles.sort_by(|a, b| a.bundle_id.cmp(&b.bundle_id));
        profiles
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SignerApp, SignerMode};

    fn profile(bundle_id: &str) -> AppProfile {
        let mut options = SignerOptions {
            custom_name: Some("Renamed".into()),
            custom_url_schemes: Some(vec!["example".into()]),
            mode: SignerMode::Adhoc,
            app: SignerApp::Feather,
            ..Default::default()
        };
        options.features.support_file_sharing = true;
        options.embedding.single_profile = true;
        AppProfile {
            bundle_id: bundle_id.into(),
            options,
        }
    }

    #[test]
    fn profiles_round_trip_and_are_listed() {
        let temp = tempfile::tempdir().unwrap();
        let profiles = AppProfiles::new(temp.path());
        assert!(profiles.load("com.example.app").is_none());

        profiles.save(&profile("com.example.app")).unwrap();
        profiles.save(&profile("com.example.another")).unwrap();
        let loaded = profiles.load("com.example.app").unwrap();
        assert_eq!(loaded.options.custom_name.as_deref(), Some("Renamed"));
        assert_eq!(loaded.options.mode, SignerMode::Adhoc);
        assert!(loaded.options.features.support_file_sharing);
        assert!(loaded.options.embedding.single_profile);
        assert_eq!(profiles.list().len(), 2);

        profiles.delete("com.example.app").unwrap();
        assert!(profiles.load("com.example.app").is_none());
        profiles.delete("com.example.app").unwrap();
    }

    #[test]
    fn hostile_bundle_identifiers_stay_in_the_folder() {
        let temp = tempfile::tempdir().unwrap();
        let profiles = AppProfiles::new(temp.path());
        profiles.save(&profile("../../escape/x")).unwrap();
        assert_eq!(profiles.list().len(), 1);
        assert!(!temp.path().join("escape").exists());
    }

    #[test]
    fn applying_keeps_the_detected_app_and_drops_missing_files() {
        let temp = tempfile::tempdir().unwrap();
        let tweak = temp.path().join("tweak.dylib");
        std::fs::write(&tweak, b"").unwrap();
        let gone = temp.path().join("gone.dylib");

        let mut saved = profile("com.example.app");
        saved.options.tweaks = Some(vec![tweak.clone(), gone.clone()]);
        let mut options = SignerOptions {
            app: SignerApp::SideStore,
            ..Default::default()
        };

        assert_eq!(saved.apply_to(&mut options), [gone]);
        assert_eq!(options.app, SignerApp::SideStore);
        assert_eq!(options.tweaks, Some(vec![tweak]));
        assert_eq!(options.custom_name.as_deref(), Some("Renamed"));
    }

    #[test]
    fn profiles_from_older_versions_fill_in_new_options() {
        let profile: AppProfile = serde_json::from_str(
            r#"{"bundle_id":"a.b","options":{"custom_name":"Old","future_option":1}}"#,
        )
        .unwrap();
        assert_eq!(profile.options.custom_name.as_deref(), Some("Old"));
        assert_eq!(profile.options.mode, SignerMode::Pem);
    }
}
