use crate::{
    Error, Result,
    developer_services::DeveloperServicesCertificate,
    installation::{AppInstaller, Connection, ConnectionPreferences},
    invalid,
    signer::{AutoSigner, SigningContext, TargetDevice},
    utilities::{ZIPCompressor, copy_directory},
};
use futures::future::BoxFuture;
use std::{path::Path, sync::Arc};

pub use crate::installation::ConnectionMessage as InstallerMessage;

pub trait IntegratedInstallerDelegate: Send + Sync {
    fn set_presented_message(&self, _message: Option<InstallerMessage>) {}
    fn installer_did_update(&self, stage: &str, progress: Option<f64>);
    fn confirm_revocation(
        &self,
        _certificates: Vec<DeveloperServicesCertificate>,
    ) -> BoxFuture<'static, bool> {
        Box::pin(async { true })
    }
}

pub struct IntegratedInstaller {
    pub context: SigningContext,
    pub delegate: Arc<dyn IntegratedInstallerDelegate>,
    installing: tokio::sync::Mutex<()>,
}
impl IntegratedInstaller {
    pub fn new(context: SigningContext, delegate: Arc<dyn IntegratedInstallerDelegate>) -> Self {
        Self {
            context,
            delegate,
            installing: tokio::sync::Mutex::new(()),
        }
    }
    pub async fn sign_in_place(
        &self,
        app: &Path,
        target_device: Option<TargetDevice>,
    ) -> Result<String> {
        let _guard = self
            .installing
            .try_lock()
            .map_err(|_| Error::AlreadyInstalling)?;
        self.sign(app, target_device).await
    }
    async fn sign(&self, app: &Path, target_device: Option<TargetDevice>) -> Result<String> {
        let mut context = self.context.clone();
        if target_device.is_some() {
            context.target_device = target_device;
        }
        let stage = std::sync::Mutex::new(String::from("Provisioning"));
        let delegate = self.delegate.clone();
        AutoSigner { context }
            .sign(
                app,
                &move |certs| delegate.confirm_revocation(certs),
                &|next| {
                    if let Ok(mut stage) = stage.lock() {
                        *stage = next.to_owned();
                    }
                    self.delegate.installer_did_update(next, None);
                },
                &|progress| {
                    if let Ok(stage) = stage.lock() {
                        self.delegate.installer_did_update(&stage, progress);
                    }
                },
                &|| Ok(()),
            )
            .await
    }
    /// Unpacks, provisions, signs, repackages and installs an IPA or app bundle.
    pub async fn install(
        &self,
        app: &Path,
        udid: &str,
        preferences: ConnectionPreferences,
        configure_device: bool,
    ) -> Result<String> {
        let _guard = self
            .installing
            .try_lock()
            .map_err(|_| Error::AlreadyInstalling)?;
        self.delegate.installer_did_update("Unpacking app", None);
        let staging = tempfile::tempdir()?;
        let payload = staging.path().join("Payload");
        match app
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("ipa") => ZIPCompressor::decompress(app, staging.path(), &|v| {
                self.delegate.installer_did_update("Unpacking app", v)
            })?,
            Some("app") => copy_directory(
                app,
                &payload.join(app.file_name().ok_or_else(|| invalid("Invalid app path"))?),
            )?,
            _ => return Err(invalid("Expected an .ipa or .app")),
        }
        let apps = std::fs::read_dir(&payload)?
            .map(|e| e.map(|e| e.path()))
            .collect::<std::io::Result<Vec<_>>>()?
            .into_iter()
            .filter(|p| p.is_dir() && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("app")))
            .collect::<Vec<_>>();
        if apps.len() != 1 {
            return Err(invalid("An IPA must contain exactly one main app"));
        }
        self.delegate
            .installer_did_update("Preparing device", Some(0.0));
        let connection = self.connect_with_recovery(udid, preferences).await?;
        let name = connection.device_name().await?;
        if configure_device {
            // Preserve the separate record for the caller's provisioning workflow.
            // The source installer likewise does not embed this record into the app.
            let _pairing_record = connection.configure_wireless().await?;
        }
        self.delegate
            .installer_did_update("Preparing device", Some(1.0));
        let bundle_id = self
            .sign(
                &apps[0],
                Some(TargetDevice {
                    udid: udid.into(),
                    name,
                }),
            )
            .await?;
        let ipa = staging.path().join("signed.ipa");
        self.delegate.installer_did_update("Packaging", None);
        ZIPCompressor::compress(&payload, &ipa, &|v| {
            self.delegate.installer_did_update("Packaging", v)
        })?;
        AppInstaller {
            ipa,
            udid: udid.into(),
            connection_preferences: preferences,
        }
        .install_with_connection(&connection, &|stage| {
            self.delegate
                .installer_did_update(stage.display_name(), Some(stage.display_progress()))
        })
        .await?;
        Ok(bundle_id)
    }
    async fn connect_with_recovery(
        &self,
        udid: &str,
        preferences: ConnectionPreferences,
    ) -> Result<Arc<Connection>> {
        struct ClearMessage<'a>(&'a dyn IntegratedInstallerDelegate);
        impl Drop for ClearMessage<'_> {
            fn drop(&mut self) {
                self.0.set_presented_message(None);
            }
        }
        let _clear = ClearMessage(self.delegate.as_ref());
        let mut previous = None;
        loop {
            let message = match Connection::connection_with_messages(
                udid,
                preferences,
                &|v| {
                    self.delegate
                        .installer_did_update("Preparing device", Some(v))
                },
                &|message| self.delegate.set_presented_message(message),
            )
            .await
            {
                Ok(connection) => return Ok(connection),
                Err(Error::Device(idevice::IdeviceError::PairingDialogResponsePending)) => {
                    InstallerMessage::PairDevice
                }
                Err(Error::Device(idevice::IdeviceError::PasswordProtected)) => {
                    InstallerMessage::UnlockDevice
                }
                Err(error) => return Err(error),
            };
            if previous != Some(message) {
                self.delegate.set_presented_message(Some(message));
                previous = Some(message);
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }
}
