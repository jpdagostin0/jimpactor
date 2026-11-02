//! Signing and installing an IPA, shared by the GUI and the CLI. Everything that talks to the
//! user goes through [`InstallHost`].

use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::future::BoxFuture;
use plume_core::{CertificateIdentity, MobileProvision, developer::DeveloperSession};
use plume_store::{AccountStore, GsaAccount, RefreshApp, RefreshDevice};

use crate::refresh::refresh_time;
use crate::sources::{AppVersion, SourceApp};
use crate::{
    AppProfiles, Bundle, ConfirmRevocation, Device, Package, PlistInfoTrait, Signer,
    SignerInstallMode, SignerMode, SignerOptions, XkitSigner,
};

pub const CERTIFICATE_RESET_WARNING: &str = "Jimpactor needs to reset your certificate. This breaks existing SideStore and AltStore installs.";

/// What an installation needs from whoever started it.
pub trait InstallHost: Send + Sync {
    /// A progress update. `percent` runs to 100; -1 is never sent here, errors are returned.
    fn status(&self, message: String, percent: i32);
    /// Whether the certificate may be reset. May block while the user answers.
    fn confirm_certificate_reset(&self) -> bool;
    /// Where to save an exported IPA, or `None` to skip saving.
    fn export_path(&self, suggested_name: String) -> BoxFuture<'_, Option<PathBuf>>;
}

fn revocation_prompt(host: Arc<dyn InstallHost>) -> Box<ConfirmRevocation> {
    Box::new(move |_certificates| {
        let host = host.clone();
        Box::pin(async move {
            host.status(CERTIFICATE_RESET_WARNING.to_string(), 20);
            tokio::task::spawn_blocking(move || host.confirm_certificate_reset())
                .await
                .unwrap_or(false)
        })
    })
}

pub async fn run_installation(
    package: &Package,
    device: Option<&Device>,
    options: &SignerOptions,
    account: Option<&GsaAccount>,
    mut store: Option<&mut AccountStore>,
    data_path: &Path,
    host: &Arc<dyn InstallHost>,
) -> Result<(), String> {
    let remote_pairing = store.as_ref().is_some_and(|s| s.remote_pairing());
    let mut pairing_error = None;

    if options.mode == SignerMode::ModifyOnly && options.install_mode != SignerInstallMode::Export {
        return Err("Modify without signing is available for export only. Choose Export.".into());
    }
    let package_file: Bundle;
    let mut options = options.clone();
    let send = |msg: String, progress: i32| host.status(msg, progress);

    if options.install_mode == SignerInstallMode::Install
        && let Some(dev) = device.filter(|dev| !dev.is_mac)
    {
        preflight_device(dev, &options, &send).await?;
    }

    send("Preparing package...".to_string(), 10);

    match options.mode {
        SignerMode::Pem => {
            let Some(account) = account else {
                return Err("GSA account is required for PEM signing".to_string());
            };

            send("Ensuring account is valid...".to_string(), 20);

            let session = DeveloperSession::new(
                account.adsid().clone(),
                account.xcode_gs_token().clone(),
                plume_store::AnisetteSettings::load_configuration(data_path),
            )
            .await
            .map_err(|e| e.to_string())?;

            let teams_response = session.qh_list_teams().await.map_err(|e| e.to_string())?;

            if teams_response.teams.is_empty() {
                return Err("No teams available for this account".to_string());
            }

            let team_id = account.team_id();

            if !team_id.is_empty() && !teams_response.teams.iter().any(|t| &t.team_id == team_id) {
                return Err(format!(
                    "Stored team ID '{}' not found in available teams. Please update your team selection in Settings.",
                    team_id
                ));
            }

            let team_id = if team_id.is_empty() {
                if teams_response.teams.len() != 1 {
                    return Err(
                        "Choose a developer team in Settings > Accounts before signing.".into(),
                    );
                }
                &teams_response.teams[0].team_id
            } else {
                team_id
            };

            let bundle = package.get_package_bundle().map_err(|e| e.to_string())?;

            if XkitSigner::supports(&options) {
                send("Ensuring device is registered...".to_string(), 30);

                let xkit_signer = XkitSigner::new(
                    account.adsid(),
                    account.xcode_gs_token(),
                    plume_store::AnisetteSettings::load_configuration(data_path),
                    team_id,
                    data_path,
                )
                .await
                .map_err(|e| e.to_string())?;

                let mut signer = Signer::new(None, options.clone());

                send("Preparing package...".to_string(), 50);

                signer
                    .modify_bundle(&bundle, &Some(team_id.clone()))
                    .await
                    .map_err(|e| e.to_string())?;

                let confirm = revocation_prompt(host.clone());
                let status_host = host.clone();
                // xkit reports stage names with 0..1 progress; map them onto 50..90.
                let status = move |stage: &str, progress: Option<f64>| {
                    let percent = 50 + (progress.unwrap_or(0.0).clamp(0.0, 1.0) * 40.0) as i32;
                    status_host.status(format!("{stage}..."), percent);
                };
                let device_info = device
                    .filter(|d| !d.udid.is_empty())
                    .map(|d| (d.udid.as_str(), d.name.as_str()));

                xkit_signer
                    .provision_and_sign(&bundle, device_info, &*confirm, &status)
                    .await
                    .map_err(|e| e.to_string())?;

                options = signer.options.clone();
                package_file = bundle;
            } else {
                let mut on_certificate_reset = || {
                    send(CERTIFICATE_RESET_WARNING.to_string(), 20);
                    host.confirm_certificate_reset()
                };
                let identity = CertificateIdentity::new_with_session(
                    &session,
                    data_path.to_path_buf(),
                    None,
                    team_id,
                    false,
                    Some(&mut on_certificate_reset),
                )
                .await
                .map_err(|e| e.to_string())?;

                send("Ensuring device is registered...".to_string(), 30);

                if let Some(dev) = &device {
                    session
                        .qh_ensure_device(team_id, &dev.name, &dev.udid)
                        .await
                        .map_err(|e| e.to_string())?;
                }

                send("Extracting package...".to_string(), 50);

                let mut signer = Signer::new(Some(identity), options.clone());

                send("Signing package...".to_string(), 70);

                signer
                    .modify_bundle(&bundle, &Some(team_id.clone()))
                    .await
                    .map_err(|e| e.to_string())?;
                signer
                    .register_bundle(&bundle, &session, team_id, false)
                    .await
                    .map_err(|e| e.to_string())?;
                signer
                    .sign_bundle(&bundle)
                    .await
                    .map_err(|e| e.to_string())?;

                options = signer.options.clone();
                package_file = bundle;
            }
        }
        SignerMode::DeveloperKey => {
            let key = store
                .as_ref()
                .and_then(|store| store.asc_key().cloned())
                .ok_or("Add an App Store Connect key in Settings > Developer first.")?;
            if !XkitSigner::supports(&options) {
                return Err(format!(
                    "{} or custom entitlements need Apple ID signing, not an App Store Connect key.",
                    options.app
                ));
            }

            let xkit_signer =
                XkitSigner::with_asc_key(&key, data_path).map_err(|e| e.to_string())?;
            // Paid teams are never asked to revoke; the certificate limit is reported by Apple.
            let confirm: Box<ConfirmRevocation> =
                Box::new(|_certificates| Box::pin(async { false }));

            send("Ensuring certificate...".to_string(), 20);
            let team_id = xkit_signer
                .signing_team(&*confirm)
                .await
                .map_err(|e| e.to_string())?;

            send("Preparing package...".to_string(), 50);
            let bundle = package.get_package_bundle().map_err(|e| e.to_string())?;
            let mut signer = Signer::new(None, options.clone());
            signer
                .modify_bundle(&bundle, &Some(team_id))
                .await
                .map_err(|e| e.to_string())?;

            let status_host = host.clone();
            let status = move |stage: &str, progress: Option<f64>| {
                let percent = 50 + (progress.unwrap_or(0.0).clamp(0.0, 1.0) * 40.0) as i32;
                status_host.status(format!("{stage}..."), percent);
            };
            let device_info = device
                .filter(|d| !d.udid.is_empty())
                .map(|d| (d.udid.as_str(), d.name.as_str()));
            xkit_signer
                .provision_and_sign(&bundle, device_info, &*confirm, &status)
                .await
                .map_err(|e| e.to_string())?;

            options = signer.options.clone();
            package_file = bundle;
        }
        SignerMode::Adhoc | SignerMode::ModifyOnly => {
            send("Extracting package...".to_string(), 50);

            let mut signer = Signer::new(None, options.clone());

            let bundle = package.get_package_bundle().map_err(|e| e.to_string())?;

            send("Signing package...".to_string(), 70);

            signer
                .modify_bundle(&bundle, &None)
                .await
                .map_err(|e| e.to_string())?;
            signer
                .sign_bundle(&bundle)
                .await
                .map_err(|e| e.to_string())?;

            options = signer.options.clone();
            package_file = bundle;
        }
        _ => {
            send("Extracting package...".to_string(), 50);

            let bundle = package.get_package_bundle().map_err(|e| e.to_string())?;

            package_file = bundle;
        }
    }

    match options.install_mode {
        SignerInstallMode::Install => {
            if let Some(dev) = &device {
                if !dev.is_mac {
                    send("Sending to device...".to_string(), 70);

                    let progress_host = host.clone();
                    dev.install_app(package_file.bundle_dir(), move |progress: i32| {
                        let host = progress_host.clone();
                        async move {
                            host.status("Installing...".to_string(), 70 + (progress / 5));
                        }
                    })
                    .await
                    .map_err(|e| format!("Install error: {}", e))?;

                    if places_pairing_file(&options)
                        && let Some(custom_identifier) = options.custom_identifier.as_ref()
                    {
                        send("Placing pairing file...".to_string(), 90);
                        if let Err(e) = place_pairing_files(
                            dev,
                            custom_identifier,
                            options.app,
                            remote_pairing,
                            data_path,
                        )
                        .await
                        {
                            log::error!("Failed to place pairing file: {e}");
                            pairing_error = Some(e.to_string());
                        }
                    }
                } else {
                    send("Installing...".to_string(), 90);

                    crate::install_app_mac(package_file.bundle_dir())
                        .await
                        .map_err(|e| e.to_string())?;
                }
            } else {
                return Err("No device connected for installation".to_string());
            }
        }
        SignerInstallMode::Export => {
            send("Exporting...".to_string(), 90);

            let archive_path = package
                .get_archive_based_on_path(package_file.bundle_dir())
                .map_err(|e| e.to_string())?;

            let suggested = archive_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("package.ipa")
                .to_owned();
            if let Some(save_path) = host.export_path(suggested).await {
                tokio::fs::copy(&archive_path, &save_path)
                    .await
                    .map_err(|e| e.to_string())?;
            }
        }
    }

    if options.refresh && options.mode == SignerMode::Pem {
        send("Saving for refresh...".to_string(), 75);
        let path = data_path.join("refresh_store");
        tokio::fs::create_dir_all(&path)
            .await
            .map_err(|e| e.to_string())?;

        let original_name = package_file
            .bundle_dir()
            .file_name()
            .unwrap()
            .to_string_lossy();
        let uuid = uuid::Uuid::new_v4();
        let dest_name = if let Some(dot_pos) = original_name.rfind('.') {
            let (name, ext) = original_name.split_at(dot_pos);
            format!("{}-{}{}", name, uuid, ext)
        } else {
            format!("{}-{}", original_name, uuid)
        };
        let dest_path = path.join(dest_name);

        crate::copy_dir_recursively(package_file.bundle_dir(), &dest_path)
            .await
            .map_err(|e| e.to_string())?;

        if let (Some(dev), Some(account), Some(store)) = (&device, &account, store.as_mut()) {
            let embedded_prov_path = dest_path.join("embedded.mobileprovision");

            let provision_path = if embedded_prov_path.exists() {
                Some(embedded_prov_path)
            } else {
                None
            };

            if let Some(prov_path) = provision_path
                && let Ok(provision) = MobileProvision::load_with_path(&prov_path)
            {
                let scheduled_refresh = refresh_time(&provision);

                let refresh_app = RefreshApp {
                    name: package_file.get_name(),
                    bundle_id: package_file.get_bundle_identifier(),
                    path: dest_path.clone(),
                    scheduled_refresh,
                };

                let mut refresh_device = store
                    .get_refresh_device(&dev.udid)
                    .cloned()
                    .unwrap_or_else(|| RefreshDevice {
                        udid: dev.udid.clone(),
                        name: dev.name.clone(),
                        account: account.email().clone(),
                        apps: Vec::new(),
                        is_mac: dev.is_mac,
                    });

                refresh_device.name = dev.name.clone();
                refresh_device.is_mac = dev.is_mac;

                refresh_device.record_install(account.email().clone(), refresh_app);

                store
                    .add_or_update_refresh_device_sync(refresh_device)
                    .map_err(|e| e.to_string())?;
            }
        }
    }

    if let Some(e) = pairing_error {
        return Err(format!(
            "The app was installed, but placing its pairing file failed: {e}"
        ));
    }

    send("Finished!".to_string(), 100);

    Ok(())
}

fn places_pairing_file(options: &crate::SignerOptions) -> bool {
    options.app.supports_pairing_file() && options.custom_identifier.is_some()
}

/// Catches device problems before signing, which would otherwise surface only after
/// installation as an app that will not open or cannot refresh.
async fn preflight_device(
    dev: &Device,
    options: &crate::SignerOptions,
    send: &impl Fn(String, i32),
) -> Result<(), String> {
    send("Checking Developer Mode...".to_string(), 3);
    if dev.developer_mode_enabled().await == Some(false) {
        if let Err(e) = dev.reveal_developer_mode_option().await {
            log::warn!(
                "Could not reveal the Developer Mode option on {}: {e}",
                dev.name
            );
        }
        return Err(format!(
            "Developer Mode is off on {}, so sideloaded apps will not open. Turn it on in Settings > Privacy & Security > Developer Mode, restart the device when asked, then retry.",
            dev.name
        ));
    }

    // The signer sets `custom_identifier` later, so only the app type is known here.
    if options.app.supports_pairing_file() {
        send("Checking device trust...".to_string(), 6);
        dev.lockdown_pairing_file().await.map_err(|e| {
            format!(
                "{} needs this computer's pairing record, which could not be read: {e}",
                options.app
            )
        })?;
    }

    Ok(())
}

/// How long to wait for an automatic RPPairing record before using lockdown pairing only.
const AUTO_REMOTE_PAIRING_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Places `app`'s pairing files. Apps that read a separate RPPairing record (SideStore)
/// get one even when remote pairing is off, falling back to the lockdown record if the
/// device cannot provide it, e.g. before iOS 17.
async fn place_pairing_files(
    dev: &Device,
    identifier: &str,
    app: crate::SignerApp,
    remote_pairing: bool,
    data_path: &Path,
) -> Result<(), crate::Error> {
    if remote_pairing || app.extra_pairing_file_paths().is_empty() {
        return dev
            .install_pairing_files(identifier, app, remote_pairing, data_path.to_path_buf())
            .await;
    }

    let attempt = tokio::time::timeout(
        AUTO_REMOTE_PAIRING_TIMEOUT,
        dev.install_pairing_files(identifier, app, true, data_path.to_path_buf()),
    )
    .await;
    match attempt {
        Ok(Ok(())) => return Ok(()),
        Ok(Err(e)) => log::warn!("RPPairing for {app} failed, using lockdown pairing: {e}"),
        Err(_) => log::warn!("RPPairing for {app} timed out, using lockdown pairing"),
    }
    dev.install_pairing_files(identifier, app, false, data_path.to_path_buf())
        .await
}

/// Downloads `version` of a source app and installs it with the default options for that app,
/// placing its pairing file afterwards (e.g. SideStore's one-click install).
pub async fn run_source_install(
    app: &SourceApp,
    version: &AppVersion,
    device: Option<&Device>,
    account: Option<&GsaAccount>,
    store: Option<&mut AccountStore>,
    data_path: &Path,
    host: &Arc<dyn InstallHost>,
) -> Result<(), String> {
    if device.is_none() {
        return Err("Connect a device to install apps".to_string());
    }

    let downloads = data_path.join("downloads");
    let file_name: String = format!("{}-{}.ipa", app.name, version.version)
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || "+-._".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let ipa_path = downloads.join(file_name);

    log::info!(
        "Downloading {} {} from {}",
        app.name,
        version.version,
        version.download_url
    );
    host.status(
        format!("Downloading {} {}...", app.name, version.version),
        0,
    );
    crate::sources::download(&version.download_url, &ipa_path, |percent| {
        host.status(
            format!("Downloading {} ({percent}%)...", app.name),
            percent / 10,
        );
    })
    .await?;

    let package = Package::new(ipa_path.clone());
    let _ = tokio::fs::remove_file(&ipa_path).await;
    let package = package.map_err(|e| format!("Downloaded file is not a valid IPA: {e}"))?;

    let mut options = SignerOptions::default();
    package.load_into_signer_options(&mut options);
    // Updates keep whatever was saved for the app, such as its name and tweaks.
    if let Some(profile) = package
        .get_bundle_identifier()
        .and_then(|bundle_id| AppProfiles::new(data_path).load(&bundle_id))
    {
        for path in profile.apply_to(&mut options) {
            log::warn!("Saved profile file is gone: {}", path.display());
        }
    }
    options.mode = if account.is_none() && store.as_ref().is_some_and(|s| s.asc_key().is_some()) {
        SignerMode::DeveloperKey
    } else {
        SignerMode::Pem
    };
    options.install_mode = SignerInstallMode::Install;

    let result =
        run_installation(&package, device, &options, account, store, data_path, host).await;
    package.remove_package_stage();
    result
}
