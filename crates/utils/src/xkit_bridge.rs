//! Provisions and signs bundles with `xkit` using an account already stored by `plume_store`.
//!
//! Bundle edits (rename, tweaks, identifier rewrites) stay in [`crate::Signer::modify_bundle`];
//! this only replaces registration, certificate management and code signing.
use std::{path::Path, sync::Arc};

use futures::future::BoxFuture;
use plume_core::{AnisetteConfiguration, auth::anisette_data::AnisetteData};
use tokio::sync::Mutex;
use xkit::{
    AutoSigner, DeveloperAPIAuthData, DeveloperAPIClient, DeveloperServicesLoginToken,
    IdentifierPolicy, SigningContext, TargetDevice, XcodeAuthData,
    developer_services::DeveloperServicesCertificate,
    grand_slam::SharedAnisetteProvider,
    utilities::{DirectoryStorage, KeyValueSigningInfoManager, SigningInfoManager},
};

use crate::{Bundle, Error, SignerApp, SignerOptions};

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
