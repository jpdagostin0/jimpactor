use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use chrono::Utc;
use plume_core::{CertificateIdentity, MobileProvision, developer::DeveloperSession};
use plume_store::{AccountStore, AnisetteSettings, RefreshDevice};
use plume_utils::{
    Bundle, ConfirmRevocation, Device, Signer, SignerMode, SignerOptions, XkitSigner,
};

use crate::defaults::get_data_path;

pub type ConnectedDevices = Arc<Mutex<HashMap<String, Device>>>;

static ACTIVE_REFRESHES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
pub(crate) fn has_active_refreshes() -> bool {
    ACTIVE_REFRESHES.load(std::sync::atomic::Ordering::SeqCst) != 0
}

struct RefreshGuard {
    udid: String,
    tasks: Arc<Mutex<HashSet<String>>>,
}

impl Drop for RefreshGuard {
    fn drop(&mut self) {
        ACTIVE_REFRESHES.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        if let Ok(mut tasks) = self.tasks.lock() {
            tasks.remove(&self.udid);
            log::debug!("Released lock for device {}", self.udid);
        }
    }
}

pub struct RefreshDaemon {
    store_path: std::path::PathBuf,
    connected_devices: ConnectedDevices,
    active_tasks: Arc<Mutex<HashSet<String>>>,
    check_interval: Duration,
}

impl RefreshDaemon {
    pub fn new() -> Self {
        Self {
            store_path: get_data_path().join("accounts.json"),
            connected_devices: Arc::new(Mutex::new(HashMap::new())),
            active_tasks: Arc::new(Mutex::new(HashSet::new())),
            check_interval: Duration::from_secs(60 * 3), // Check every 3 minutes
        }
    }

    pub fn connected_devices(&self) -> ConnectedDevices {
        self.connected_devices.clone()
    }

    pub fn spawn(self) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            loop {
                if let Err(e) = rt.block_on(self.check_and_refresh()) {
                    log::error!("Refresh daemon error: {}", e);
                    notify_rust::Notification::new()
                        .summary(crate::APP_NAME)
                        .body(&format!("Failed to refresh: {}", e))
                        .show()
                        .ok();
                }

                thread::sleep(self.check_interval);
            }
        })
    }

    async fn check_and_refresh(&self) -> Result<(), String> {
        let store = AccountStore::load(&Some(self.store_path.clone()))
            .await
            .map_err(|e| format!("Failed to load account store: {}", e))?;

        let now = Utc::now();

        for (udid, refresh_device) in store.refreshes() {
            for app in &refresh_device.apps {
                if app.scheduled_refresh <= now {
                    // We check for active tasks here to prevent the background loop
                    // from even starting a wait if a manual refresh is already running.
                    if self.is_busy(udid) {
                        log::info!(
                            "Device {} is already being processed. Skipping this app for now.",
                            udid
                        );
                        continue;
                    }

                    log::info!("App at {:?} needs refresh for device {}", app.path, udid);

                    let device = self
                        .connected_devices
                        .lock()
                        .ok()
                        .and_then(|devices| devices.get(udid).cloned());

                    let Some(device) = device else {
                        log::debug!(
                            "App at {:?} is due for refresh on {}, but no matching connected device was found. Retrying in {} seconds.",
                            app.path,
                            udid,
                            self.check_interval.as_secs()
                        );
                        continue;
                    };

                    if let Err(e) = self.refresh_app(&store, refresh_device, app, &device).await {
                        log::error!("Error refreshing app: {}", e);
                    }
                }
            }
        }

        Ok(())
    }

    fn is_busy(&self, udid: &str) -> bool {
        self.active_tasks
            .lock()
            .map(|t| t.contains(udid))
            .unwrap_or(false)
    }

    pub async fn refresh_app(
        &self,
        store: &AccountStore,
        refresh_device: &RefreshDevice,
        app: &plume_store::RefreshApp,
        device: &Device,
    ) -> Result<(), String> {
        // Try to acquire the lock for this UDID.
        {
            let mut tasks = self
                .active_tasks
                .lock()
                .map_err(|_| "Failed to lock task registry")?;
            if tasks.contains(&device.udid) {
                log::warn!(
                    "Refresh already in progress for {}. Aborting duplicate.",
                    device.udid
                );
                return Ok(());
            }
            tasks.insert(device.udid.clone());
        }

        // lock is released when this function returns
        ACTIVE_REFRESHES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let _guard = RefreshGuard {
            udid: device.udid.clone(),
            tasks: self.active_tasks.clone(),
        };

        log::info!("Starting refresh for app at {:?}", app.path);

        notify_rust::Notification::new()
            .summary(crate::APP_NAME)
            .body(&format!(
                "Started refreshing {} for {}",
                app.name.as_deref().unwrap_or("???"),
                refresh_device.name
            ))
            .show()
            .ok();

        let account = store
            .get_account(&refresh_device.account)
            .ok_or_else(|| format!("Account {} not found", refresh_device.account))?;

        let session = DeveloperSession::new(
            account.adsid().clone(),
            account.xcode_gs_token().clone(),
            AnisetteSettings::load_configuration(&get_data_path()),
        )
        .await
        .map_err(|e| format!("Failed to create session: {}", e))?;

        let teams_response = session
            .qh_list_teams()
            .await
            .map_err(|e| format!("Failed to list teams: {}", e))?;

        if teams_response.teams.is_empty() {
            return Err("No teams available for this account".to_string());
        }

        let team_id = if account.team_id().is_empty() {
            &teams_response.teams[0].team_id
        } else {
            account.team_id()
        };

        // SideStore-family apps embed their signing certificate and must keep the legacy signer.
        let xkit_signer = if embeds_certificate(&app.path) {
            None
        } else {
            Some(
                XkitSigner::new(
                    account.adsid(),
                    account.xcode_gs_token(),
                    AnisetteSettings::load_configuration(&get_data_path()),
                    team_id,
                    &get_data_path(),
                )
                .await
                .map_err(|e| format!("Failed to create signing context: {}", e))?,
            )
        };

        let identity_is_new = if let Some(xkit_signer) = &xkit_signer {
            xkit_signer
                .ensure_certificate(&*confirm_revocation())
                .await
                .map_err(|e| format!("Failed to create identity: {}", e))?
        } else {
            let mut on_certificate_reset = crate::certificate_reset::confirm;
            let identity = CertificateIdentity::new_with_session(
                &session,
                get_data_path(),
                None,
                team_id,
                false,
                Some(&mut on_certificate_reset),
            )
            .await
            .map_err(|e| format!("Failed to create identity: {}", e))?;
            identity.new
        };

        let is_installed = if let Some(bundle_id) = app.bundle_id.as_deref() {
            device
                .is_app_installed(bundle_id)
                .await
                .map_err(|e| format!("Failed to check if app is installed: {}", e))?
        } else {
            false
        };

        // Determine if we need to reinstall:
        // - Mac devices always need reinstalling
        // - If the identity is new, we need to reinstall
        // - If the app is not installed, we need to reinstall
        // - If the app is installed and identity is not new, we can just update profiles
        let needs_reinstall = device.is_mac || identity_is_new || !is_installed;

        if needs_reinstall {
            self.resign_and_reinstall(app, device, &session, team_id, xkit_signer.as_ref())
                .await?;
        } else {
            log::info!(
                "Certificate exists and app is installed, updating provisioning profiles..."
            );
            self.update_provisioning_profiles(app, device, &session, team_id)
                .await?;
        }

        self.update_refresh_schedule(store, refresh_device, app)
            .await?;

        log::info!("Successfully refreshed app at {:?}", app.path);

        notify_rust::Notification::new()
            .summary(crate::APP_NAME)
            .body(&format!(
                "Successfully refreshed {} for {}",
                app.name.as_deref().unwrap_or("???"),
                refresh_device.name
            ))
            .show()
            .ok();

        Ok(())
    }

    async fn resign_and_reinstall(
        &self,
        app: &plume_store::RefreshApp,
        device: &Device,
        session: &DeveloperSession,
        team_id: &str,
        xkit_signer: Option<&XkitSigner>,
    ) -> Result<(), String> {
        let bundle =
            Bundle::new(app.path.clone()).map_err(|e| format!("Failed to create bundle: {}", e))?;

        if let Some(xkit_signer) = xkit_signer {
            let device_info =
                (!device.is_mac).then_some((device.udid.as_str(), device.name.as_str()));
            xkit_signer
                .provision_and_sign(&bundle, device_info, &*confirm_revocation(), &|_, _| {})
                .await
                .map_err(|e| format!("Failed to sign bundle: {}", e))?;
        } else {
            let team_id_string = team_id.to_string();
            session
                .qh_ensure_device(&team_id_string, &device.name, &device.udid)
                .await
                .map_err(|e| format!("Failed to ensure device: {}", e))?;
            self.legacy_sign(&bundle, session, team_id).await?;
        }

        self.install(app, device).await
    }

    async fn legacy_sign(
        &self,
        bundle: &Bundle,
        session: &DeveloperSession,
        team_id: &str,
    ) -> Result<(), String> {
        let options = SignerOptions {
            mode: SignerMode::Pem,
            ..Default::default()
        };

        let team_id_string = team_id.to_string();
        let mut on_certificate_reset = crate::certificate_reset::confirm;
        let signing_identity = CertificateIdentity::new_with_session(
            session,
            get_data_path(),
            None,
            &team_id_string,
            false,
            Some(&mut on_certificate_reset),
        )
        .await
        .map_err(|e| format!("Failed to create signing identity: {}", e))?;

        let mut signer = Signer::new(Some(signing_identity), options);

        signer
            .register_bundle(bundle, session, team_id, true)
            .await
            .map_err(|e| format!("Failed to register bundle: {}", e))?;

        signer
            .sign_bundle(bundle)
            .await
            .map_err(|e| format!("Failed to sign bundle: {}", e))?;

        Ok(())
    }

    async fn install(&self, app: &plume_store::RefreshApp, device: &Device) -> Result<(), String> {
        if !device.is_mac {
            device
                .install_app(&app.path, |_| async {})
                .await
                .map_err(|e| format!("Failed to install app: {}", e))?;
        } else {
            plume_utils::install_app_mac(&app.path)
                .await
                .map_err(|e| format!("Failed to install app on Mac: {}", e))?;
        }

        Ok(())
    }

    async fn update_provisioning_profiles(
        &self,
        app: &plume_store::RefreshApp,
        device: &Device,
        session: &DeveloperSession,
        team_id: &str,
    ) -> Result<(), String> {
        let bundle =
            Bundle::new(app.path.clone()).map_err(|e| format!("Failed to create bundle: {}", e))?;

        let options = SignerOptions {
            mode: SignerMode::Pem,
            ..Default::default()
        };

        let mut signer = Signer::new(None, options);

        signer
            .register_bundle(&bundle, session, team_id, true)
            .await
            .map_err(|e| format!("Failed to register bundle: {}", e))?;

        for provision in &signer.provisioning_files {
            device
                .install_profile(provision)
                .await
                .map_err(|e| format!("Failed to install profile: {}", e))?;
        }

        Ok(())
    }

    async fn update_refresh_schedule(
        &self,
        store: &AccountStore,
        refresh_device: &RefreshDevice,
        app: &plume_store::RefreshApp,
    ) -> Result<(), String> {
        let embedded_prov_path = app.path.join("embedded.mobileprovision");
        if !embedded_prov_path.exists() {
            return Err("embedded.mobileprovision not found".to_string());
        }

        let provision = MobileProvision::load_with_path(&embedded_prov_path)
            .map_err(|e| format!("Failed to load mobile provision: {}", e))?;

        let expiration_date = *provision.expiration_date();
        let scheduled_refresh = expiration_date
            .to_xml_format()
            .parse::<chrono::DateTime<chrono::Utc>>()
            .unwrap_or_else(|_| Utc::now() + chrono::Duration::days(4));
        let scheduled_refresh = scheduled_refresh - chrono::Duration::days(3);

        let mut store = store.clone();
        let mut updated_device = refresh_device.clone();

        if let Some(existing_app) = updated_device.apps.iter_mut().find(|a| a.path == app.path) {
            existing_app.scheduled_refresh = scheduled_refresh;
        }

        store
            .add_or_update_refresh_device_sync(updated_device)
            .map_err(|e| format!("Failed to update refresh schedule: {}", e))?;

        log::info!("Next refresh scheduled for: {}", scheduled_refresh);

        Ok(())
    }
}

fn confirm_revocation() -> Box<ConfirmRevocation> {
    Box::new(|_certificates| {
        Box::pin(async {
            tokio::task::spawn_blocking(crate::certificate_reset::confirm)
                .await
                .unwrap_or(false)
        })
    })
}

/// SideStore-family apps ship the signing certificate as a p12 next to their executable.
fn embeds_certificate(app: &std::path::Path) -> bool {
    ["ALTCertificate.p12", "Certificate.p12"]
        .iter()
        .any(|name| app.join(name).exists())
        || app
            .join("SideStoreApp.framework/ALTCertificate.p12")
            .exists()
}

pub fn spawn_refresh_daemon() -> (thread::JoinHandle<()>, ConnectedDevices) {
    let daemon = RefreshDaemon::new();
    let devices = daemon.connected_devices();
    let handle = daemon.spawn();
    (handle, devices)
}
