//! Provisions and signs bundles with `xkit` using an account already stored by `plume_store`.
//!
//! Bundle edits (rename, tweaks, identifier rewrites) stay in [`crate::Signer::modify_bundle`];
//! this only replaces registration, certificate management and code signing.
use std::{path::Path, sync::Arc};

use futures::future::BoxFuture;
use plume_core::{AnisetteConfiguration, auth::anisette_data::AnisetteData};
use tokio::sync::Mutex;
use xkit::{
    ASCKey, AutoSigner, DeveloperAPIAuthData, DeveloperAPIClient, DeveloperServicesLoginToken,
    IdentifierPolicy, SigningContext, TargetDevice, XcodeAuthData,
    developer_services::{ASCJWTGenerator, DeveloperServicesCertificate, Resource},
    grand_slam::SharedAnisetteProvider,
    utilities::{DirectoryStorage, KeyValueSigningInfoManager, SigningInfoManager},
};

use crate::{Bundle, Error, SignerApp, SignerOptions};
use plume_store::AscKey;

/// Asks whether the listed development certificates may be revoked.
pub type ConfirmRevocation =
    dyn Fn(Vec<DeveloperServicesCertificate>) -> BoxFuture<'static, bool> + Send + Sync;

pub struct XkitSigner {
    context: SigningContext,
    manager: Arc<KeyValueSigningInfoManager>,
    team_id: String,
}

impl XkitSigner {
    /// Whether `options` can be satisfied by xkit. SideStore-family apps embed the signing
    /// certificate as a p12, and custom or single-profile entitlements are applied by the
    /// legacy signer, so those keep using it.
    pub fn supports(options: &SignerOptions) -> bool {
        !matches!(
            options.app,
            SignerApp::AltStore | SignerApp::SideStore | SignerApp::LiveContainerAndSideStore
        ) && options.custom_entitlements.is_none()
            && !options.embedding.single_profile
    }

    pub async fn new(
        adsid: &str,
        xcode_gs_token: &str,
        anisette: AnisetteConfiguration,
        team_id: &str,
        data_dir: &Path,
    ) -> Result<Self, Error> {
        let anisette = Arc::new(Mutex::new(AnisetteData::new(anisette).await?));
        let auth = DeveloperAPIAuthData::Xcode(XcodeAuthData {
            login_token: DeveloperServicesLoginToken {
                adsid: adsid.to_owned(),
                token: xcode_gs_token.to_owned(),
                // The stored token is validated by the caller; Apple rejects it when stale.
                expiry: chrono::Utc::now() + chrono::Duration::days(365),
            },
            team_id: team_id.to_owned(),
        });
        let client =
            DeveloperAPIClient::new(auth, Some(Arc::new(SharedAnisetteProvider(anisette))))?
                .with_identifier_policy(IdentifierPolicy::Verbatim);
        let manager = Arc::new(KeyValueSigningInfoManager {
            storage: Arc::new(DirectoryStorage::new(data_dir.join("xkit"))),
        });
        Ok(Self {
            context: SigningContext::new(client, manager.clone()),
            manager,
            team_id: team_id.to_owned(),
        })
    }

    /// Signs through the App Store Connect API. No Apple ID or anisette is involved, so this
    /// only works for paid teams, and the key's role must allow certificates and profiles.
    pub fn with_asc_key(key: &AscKey, data_dir: &Path) -> Result<Self, Error> {
        let key = ASCKey {
            id: key.key_id.clone(),
            issuer_id: key.issuer_id.clone(),
            pem: key.pem.clone(),
        };
        ASCJWTGenerator::new(key.clone())
            .generate()
            .map_err(|error| Error::Other(format!("This API key cannot be used: {error}")))?;
        let issuer_id = key.issuer_id.clone();
        let client = DeveloperAPIClient::new(DeveloperAPIAuthData::AppStoreConnect(key), None)?
            .with_identifier_policy(IdentifierPolicy::Verbatim);
        let manager = Arc::new(KeyValueSigningInfoManager {
            storage: Arc::new(DirectoryStorage::new(data_dir.join("xkit"))),
        });
        Ok(Self {
            context: SigningContext::new(client, manager.clone()),
            manager,
            team_id: issuer_id,
        })
    }

    /// The team a development certificate belongs to, issuing one if needed. An API key does
    /// not say which team it is for, so this is how its team ID is learned.
    pub async fn signing_team(&self, confirm: &ConfirmRevocation) -> Result<String, Error> {
        let info = self
            .context
            .developer_api_client
            .fetch_certificate(self.manager.as_ref(), confirm)
            .await?;
        Ok(info.certificate.team_id()?)
    }

    pub async fn certificates(&self) -> Result<Vec<DeveloperItem>, Error> {
        let client = &self.context.developer_api_client;
        Ok(client
            .certificates()
            .await?
            .iter()
            .map(describe_certificate)
            .collect())
    }

    pub async fn revoke_certificate(&self, id: &str) -> Result<(), Error> {
        Ok(self
            .context
            .developer_api_client
            .revoke_certificate(id)
            .await?)
    }

    pub async fn bundle_ids(&self) -> Result<Vec<DeveloperItem>, Error> {
        let client = &self.context.developer_api_client;
        Ok(client
            .bundle_ids()
            .await?
            .iter()
            .map(describe_bundle_id)
            .collect())
    }

    pub async fn delete_bundle_id(&self, id: &str) -> Result<(), Error> {
        Ok(self
            .context
            .developer_api_client
            .delete_bundle_id(id)
            .await?)
    }

    pub async fn profiles(&self) -> Result<Vec<DeveloperItem>, Error> {
        let client = &self.context.developer_api_client;
        Ok(client
            .profiles()
            .await?
            .iter()
            .map(describe_profile)
            .collect())
    }

    pub async fn delete_profile(&self, id: &str) -> Result<(), Error> {
        Ok(self.context.developer_api_client.delete_profile(id).await?)
    }

    pub async fn register_device(&self, udid: &str, name: &str) -> Result<(), Error> {
        let target = TargetDevice {
            udid: udid.to_owned(),
            name: name.to_owned(),
        };
        Ok(self
            .context
            .developer_api_client
            .register_device(Some(&target))
            .await?)
    }

    pub async fn registered_devices(&self) -> Result<Vec<RegisteredDevice>, Error> {
        Ok(self
            .context
            .developer_api_client
            .registered_devices()
            .await?
            .into_iter()
            .map(|device| RegisteredDevice {
                id: device.id.clone(),
                name: device.attribute("name").unwrap_or("Unnamed device").into(),
                udid: device.attribute("udid").unwrap_or_default().into(),
                status: device.attribute("status").unwrap_or("UNKNOWN").into(),
                platform: device.attribute("platform").unwrap_or_default().into(),
            })
            .collect())
    }
    pub async fn set_device_enabled(&self, id: &str, enabled: bool) -> Result<(), Error> {
        self.context
            .developer_api_client
            .set_device_enabled(id, enabled)
            .await?;
        Ok(())
    }

    /// Ensures a valid development certificate exists, returning whether a new one was issued.
    pub async fn ensure_certificate(&self, confirm: &ConfirmRevocation) -> Result<bool, Error> {
        let before = self.manager.info(&self.team_id)?;
        let after = self
            .context
            .developer_api_client
            .fetch_certificate(self.manager.as_ref(), confirm)
            .await?;
        Ok(before.is_none_or(|b| {
            !b.certificate
                .has_serial_number(&after.certificate.serial_number())
        }))
    }

    /// Registers `device`, provisions every app and extension in `bundle`, and signs it in place.
    pub async fn provision_and_sign(
        &self,
        bundle: &Bundle,
        device: Option<(&str, &str)>,
        confirm: &ConfirmRevocation,
        status: &(dyn Fn(&str, Option<f64>) + Send + Sync),
    ) -> Result<String, Error> {
        let mut context = self.context.clone();
        context.target_device = device.map(|(udid, name)| TargetDevice {
            udid: udid.to_owned(),
            name: name.to_owned(),
        });
        let stage = std::sync::Mutex::new(String::new());
        Ok(AutoSigner { context }
            .sign(
                bundle.bundle_dir(),
                confirm,
                &|next| {
                    if let Ok(mut stage) = stage.lock() {
                        *stage = next.to_owned();
                    }
                    status(next, None);
                },
                &|progress| {
                    if let Ok(stage) = stage.lock() {
                        status(&stage, progress);
                    }
                },
                &|| Ok(()),
            )
            .await?)
    }
}

/// A certificate, bundle ID or profile on the developer account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeveloperItem {
    pub id: String,
    pub name: String,
    pub detail: String,
}

/// `2026-11-02T10:00:00.000+00:00` -> `2026-11-02`.
fn day(resource: &Resource, key: &str) -> String {
    resource
        .attribute(key)
        .unwrap_or_default()
        .chars()
        .take(10)
        .collect()
}

fn describe_certificate(resource: &Resource) -> DeveloperItem {
    let name = resource
        .attribute("displayName")
        .or_else(|| resource.attribute("name"))
        .unwrap_or("Certificate");
    let kind = resource.attribute("certificateType").unwrap_or("UNKNOWN");
    DeveloperItem {
        id: resource.id.clone(),
        name: name.to_owned(),
        detail: format!("{kind} · expires {}", day(resource, "expirationDate")),
    }
}

fn describe_bundle_id(resource: &Resource) -> DeveloperItem {
    DeveloperItem {
        id: resource.id.clone(),
        name: resource.attribute("name").unwrap_or("Bundle ID").to_owned(),
        detail: resource
            .attribute("identifier")
            .unwrap_or_default()
            .to_owned(),
    }
}

fn describe_profile(resource: &Resource) -> DeveloperItem {
    let state = resource.attribute("profileState").unwrap_or("UNKNOWN");
    let kind = resource.attribute("profileType").unwrap_or("UNKNOWN");
    DeveloperItem {
        id: resource.id.clone(),
        name: resource.attribute("name").unwrap_or("Profile").to_owned(),
        detail: format!(
            "{kind} · {state} · expires {}",
            day(resource, "expirationDate")
        ),
    }
}

#[derive(Clone, Debug)]
pub struct RegisteredDevice {
    pub id: String,
    pub name: String,
    pub udid: String,
    pub status: String,
    pub platform: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resource(attributes: serde_json::Value) -> Resource {
        serde_json::from_value(serde_json::json!({
            "id": "ABC123", "type": "profiles", "attributes": attributes
        }))
        .unwrap()
    }

    #[test]
    fn resources_describe_themselves_with_short_dates() {
        let item = describe_profile(&resource(serde_json::json!({
            "name": "Example",
            "profileState": "ACTIVE",
            "profileType": "IOS_APP_DEVELOPMENT",
            "expirationDate": "2027-01-02T03:04:05.000+00:00",
        })));
        assert_eq!(item.name, "Example");
        assert_eq!(
            item.detail,
            "IOS_APP_DEVELOPMENT · ACTIVE · expires 2027-01-02"
        );

        let certificate = describe_certificate(&resource(serde_json::json!({
            "name": "iOS Development", "displayName": "Created via API",
            "certificateType": "DEVELOPMENT", "expirationDate": "2027-01-02"
        })));
        assert_eq!(certificate.name, "Created via API");
    }

    #[test]
    fn a_malformed_key_is_rejected_before_any_request() {
        let key = AscKey {
            key_id: "KEY".into(),
            issuer_id: "ISSUER".into(),
            pem: "not a key".into(),
        };
        let error = XkitSigner::with_asc_key(&key, Path::new("unused"))
            .err()
            .unwrap();
        assert!(error.to_string().contains("cannot be used"));
    }
}
