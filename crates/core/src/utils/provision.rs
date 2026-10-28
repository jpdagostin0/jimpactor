use std::fs;
use std::path::{Path, PathBuf};

use crate::Error;
use crate::utils::TEAM_ID_REGEX;
use plist::{Date, Dictionary, Value};

use super::MachO;

#[derive(Clone)]
pub struct MobileProvision {
    pub data: Vec<u8>,
    entitlements: Dictionary,
    expiration_date: Date,
    metadata: Dictionary,
}

impl MobileProvision {
    pub fn load_with_path<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        let path = path.as_ref();
        let data = fs::read(path)?;

        Self::load_with_bytes(data)
    }

    pub fn load_with_bytes(data: Vec<u8>) -> Result<Self, Error> {
        let (entitlements, expiration_date, metadata) =
            Self::extract_entitlements_from_prov(&data)?;

        Ok(Self {
            data,
            entitlements,
            expiration_date,
            metadata,
        })
    }

    pub fn matches_bundle_id(&self, bundle_id: &str) -> bool {
        self.bundle_id().is_some_and(|pattern| {
            pattern == bundle_id
                || pattern
                    .strip_suffix('*')
                    .is_some_and(|prefix| bundle_id.starts_with(prefix))
        })
    }

    /// Check the profile against the app, destination and signing identity before transfer.
    pub fn validate_for(
        &self,
        bundle_id: &str,
        udid: Option<&str>,
        certificate: Option<&[u8]>,
    ) -> Result<(), Error> {
        let invalid = |reason: &str| {
            Error::Certificate(format!(
                "Provisioning profile for {bundle_id}: {reason}. Re-sign with a matching profile (0xe8008015)."
            ))
        };
        if std::time::SystemTime::from(self.expiration_date) <= std::time::SystemTime::now() {
            return Err(invalid("expired"));
        }
        if !self.matches_bundle_id(bundle_id) {
            return Err(invalid("bundle identifier does not match"));
        }
        let team = self
            .entitlements
            .get("com.apple.developer.team-identifier")
            .and_then(Value::as_string);
        let application = self
            .entitlements
            .get("application-identifier")
            .and_then(Value::as_string);
        let teams = self
            .metadata
            .get("TeamIdentifier")
            .and_then(Value::as_array);
        // App ID prefixes can differ from team IDs on older accounts. The separate
        // team entitlement must agree with TeamIdentifier when Apple supplies it.
        if team.is_none()
            || application.is_none()
            || teams.is_some_and(|teams| !teams.iter().any(|v| v.as_string() == team))
        {
            return Err(invalid("team identifier is missing or inconsistent"));
        }
        if let Some(udid) = udid {
            let all_devices = self
                .metadata
                .get("ProvisionsAllDevices")
                .and_then(Value::as_boolean)
                == Some(true);
            let registered = self
                .metadata
                .get("ProvisionedDevices")
                .and_then(Value::as_array)
                .is_some_and(|devices| devices.iter().any(|v| v.as_string() == Some(udid)));
            if !all_devices && !registered {
                return Err(invalid("destination device is not included"));
            }
        }
        if let Some(certificate) = certificate {
            let matches = self
                .metadata
                .get("DeveloperCertificates")
                .and_then(Value::as_array)
                .is_some_and(|certificates| {
                    certificates
                        .iter()
                        .any(|v| v.as_data() == Some(certificate))
                });
            if !matches {
                return Err(invalid("signing certificate is not included"));
            }
        }
        Ok(())
    }

    pub fn merge_entitlements(
        &mut self,
        binary_path: PathBuf,
        new_application_id: &str,
    ) -> Result<(), Error> {
        let macho = MachO::new(&binary_path)?;
        let binary_entitlements = macho
            .entitlements()
            .clone()
            .ok_or(Error::ProvisioningEntitlementsUnknown)?;

        let new_team_id = self
            .entitlements
            .get("com.apple.developer.team-identifier")
            .and_then(Value::as_string)
            .map(|s| s.to_owned());

        crate::utils::merge_entitlements(
            &mut self.entitlements,
            &binary_entitlements,
            &new_team_id,
            &Some(new_application_id.to_string()),
        );

        Ok(())
    }

    pub fn entitlements(&self) -> &Dictionary {
        &self.entitlements
    }

    pub fn expiration_date(&self) -> &Date {
        &self.expiration_date
    }

    pub fn entitlements_as_bytes(&self) -> Result<Vec<u8>, Error> {
        let mut buf = Vec::new();
        Value::Dictionary(self.entitlements.clone()).to_writer_xml(&mut buf)?;
        Ok(buf)
    }

    pub fn bundle_id(&self) -> Option<String> {
        let app_id = self
            .entitlements
            .get("application-identifier")?
            .as_string()?;

        let re = regex::Regex::new(TEAM_ID_REGEX).ok()?;
        let bundle_id = re.replace(app_id, "").to_string();

        Some(bundle_id)
    }

    fn extract_entitlements_from_prov(
        data: &[u8],
    ) -> Result<(Dictionary, Date, Dictionary), Error> {
        let start = data
            .windows(6)
            .position(|w| w == b"<plist")
            .ok_or(Error::ProvisioningEntitlementsUnknown)?;
        let end = data
            .windows(8)
            .rposition(|w| w == b"</plist>")
            .ok_or(Error::ProvisioningEntitlementsUnknown)?
            + 8;
        let plist_data = &data[start..end];
        let plist = plist::Value::from_reader_xml(plist_data)?;

        let expiration_date = plist
            .as_dictionary()
            .and_then(|d| d.get("ExpirationDate"))
            .and_then(|v| v.as_date());

        let entitlements = plist
            .as_dictionary()
            .and_then(|d| d.get("Entitlements"))
            .and_then(|v| v.as_dictionary())
            .cloned()
            .ok_or(Error::ProvisioningEntitlementsUnknown);

        Ok((
            entitlements?,
            expiration_date.ok_or(Error::ProvisioningEntitlementsUnknown)?,
            plist
                .as_dictionary()
                .cloned()
                .ok_or(Error::ProvisioningEntitlementsUnknown)?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn profile(pattern: &str) -> MobileProvision {
        let entitlements = Dictionary::from_iter([
            (
                "application-identifier",
                Value::String(format!("PREFIX1234.{pattern}")),
            ),
            ("com.apple.developer.team-identifier", "TEAM123456".into()),
        ]);
        MobileProvision {
            data: vec![],
            entitlements,
            expiration_date: (std::time::SystemTime::now() + std::time::Duration::from_secs(3600))
                .into(),
            metadata: Dictionary::from_iter([
                ("TeamIdentifier", Value::Array(vec!["TEAM123456".into()])),
                ("ProvisionedDevices", Value::Array(vec!["test-udid".into()])),
                (
                    "DeveloperCertificates",
                    Value::Array(vec![Value::Data(vec![1, 2, 3])]),
                ),
            ]),
        }
    }
    #[test]
    fn validates_profile_for_bundle_device_certificate_and_team() {
        let mut profile = profile("com.example.*");
        assert!(
            profile
                .validate_for("com.example.app", Some("test-udid"), Some(&[1, 2, 3]))
                .is_ok()
        );
        for (bundle, device, cert) in [
            ("com.other.app", "test-udid", &[1, 2, 3][..]),
            ("com.example.app", "other-udid", &[1, 2, 3][..]),
            ("com.example.app", "test-udid", &[4, 5, 6][..]),
        ] {
            assert!(
                profile
                    .validate_for(bundle, Some(device), Some(cert))
                    .is_err()
            );
        }
        profile.entitlements.insert(
            "com.apple.developer.team-identifier".into(),
            "OTHERTEAM1".into(),
        );
        assert!(profile.validate_for("com.example.app", None, None).is_err());
    }
    #[test]
    fn rejects_expired_profiles() {
        let mut profile = profile("com.example.app");
        profile.expiration_date = std::time::SystemTime::UNIX_EPOCH.into();
        assert!(
            profile
                .validate_for("com.example.app", None, None)
                .unwrap_err()
                .to_string()
                .contains("expired")
        );
    }
}
