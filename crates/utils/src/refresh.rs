//! Refreshing an installed app before its profile expires. Scheduling and notifications
//! belong to the caller.

use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use plume_core::{CertificateIdentity, MobileProvision, developer::DeveloperSession};
use plume_store::{AccountStore, AnisetteSettings, RefreshApp, RefreshDevice};

use crate::{
    Bundle, ConfirmRevocation, Device, Signer, SignerMode, SignerOptions, XkitSigner,
    install_app_mac,
};

/// Whether a certificate may be reset; may block while the user answers.
pub type ConfirmReset = Arc<dyn Fn() -> bool + Send + Sync>;

/// When an app with `provision` should next be refreshed: three days before it expires.
pub fn refresh_time(provision: &MobileProvision) -> DateTime<Utc> {
    let expiration = provision
        .expiration_date()
        .to_xml_format()
        .parse::<DateTime<Utc>>()
        .unwrap_or_else(|_| Utc::now() + chrono::Duration::days(4));
    expiration - chrono::Duration::days(3)
}

/// Refreshes `app` on `device` and reschedules it.
pub async fn refresh_app(
    store: &AccountStore,
    refresh_device: &RefreshDevice,
    app: &RefreshApp,
    device: &Device,
    data_path: &Path,
    confirm: ConfirmReset,
) -> Result<(), String> {
    let account = store
        .get_account(&refresh_device.account)
        .ok_or_else(|| format!("Account {} not found", refresh_device.account))?;

    let session = DeveloperSession::new(
        account.adsid().clone(),
        account.xcode_gs_token().clone(),
        AnisetteSettings::load_configuration(data_path),
    )
    .await
    .map_err(|e| format!("Failed to create session: {e}"))?;

    let teams_response = session
        .qh_list_teams()
        .await
        .map_err(|e| format!("Failed to list teams: {e}"))?;
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
                AnisetteSettings::load_configuration(data_path),
                team_id,
                data_path,
            )
            .await
            .map_err(|e| format!("Failed to create signing context: {e}"))?,
        )
    };

    let revocation = confirm_revocation(confirm.clone());
    let identity_is_new = if let Some(xkit_signer) = &xkit_signer {
        xkit_signer
            .ensure_certificate(&*revocation)
            .await
            .map_err(|e| format!("Failed to create identity: {e}"))?
    } else {
        let confirm = confirm.clone();
        let mut on_certificate_reset = move || confirm();
        CertificateIdentity::new_with_session(
            &session,
            data_path.to_path_buf(),
            None,
            team_id,
            false,
            Some(&mut on_certificate_reset),
        )
        .await
        .map_err(|e| format!("Failed to create identity: {e}"))?
        .new
    };

    let is_installed = if let Some(bundle_id) = app.bundle_id.as_deref() {
        device
            .is_app_installed(bundle_id)
            .await
            .map_err(|e| format!("Failed to check if app is installed: {e}"))?
    } else {
        false
    };

    // A Mac, a new identity or a missing app needs a full reinstall; otherwise new
    // profiles are enough.
    if device.is_mac || identity_is_new || !is_installed {
        let bundle =
            Bundle::new(app.path.clone()).map_err(|e| format!("Failed to create bundle: {e}"))?;
        if let Some(xkit_signer) = &xkit_signer {
            let device_info =
                (!device.is_mac).then_some((device.udid.as_str(), device.name.as_str()));
            xkit_signer
                .provision_and_sign(&bundle, device_info, &*revocation, &|_, _| {})
                .await
                .map_err(|e| format!("Failed to sign bundle: {e}"))?;
        } else {
            session
                .qh_ensure_device(&team_id.to_string(), &device.name, &device.udid)
                .await
                .map_err(|e| format!("Failed to ensure device: {e}"))?;
            legacy_sign(&bundle, &session, team_id, data_path, confirm).await?;
        }
        install(app, device).await?;
    } else {
        log::info!("Certificate exists and app is installed, updating provisioning profiles...");
        update_profiles(app, device, &session, team_id).await?;
    }

    reschedule(store, refresh_device, app)
}

fn confirm_revocation(confirm: ConfirmReset) -> Box<ConfirmRevocation> {
    Box::new(move |_certificates| {
        let confirm = confirm.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || confirm())
                .await
                .unwrap_or(false)
        })
    })
}

async fn legacy_sign(
    bundle: &Bundle,
    session: &DeveloperSession,
    team_id: &str,
    data_path: &Path,
    confirm: ConfirmReset,
) -> Result<(), String> {
    let options = SignerOptions {
        mode: SignerMode::Pem,
        ..Default::default()
    };
    let mut on_certificate_reset = move || confirm();
    let identity = CertificateIdentity::new_with_session(
        session,
        data_path.to_path_buf(),
        None,
        &team_id.to_string(),
        false,
        Some(&mut on_certificate_reset),
    )
    .await
    .map_err(|e| format!("Failed to create signing identity: {e}"))?;

    let mut signer = Signer::new(Some(identity), options);
    signer
        .register_bundle(bundle, session, team_id, true)
        .await
        .map_err(|e| format!("Failed to register bundle: {e}"))?;
    signer
        .sign_bundle(bundle)
        .await
        .map_err(|e| format!("Failed to sign bundle: {e}"))
}

async fn install(app: &RefreshApp, device: &Device) -> Result<(), String> {
    if device.is_mac {
        install_app_mac(&app.path)
            .await
            .map_err(|e| format!("Failed to install app on Mac: {e}"))
    } else {
        device
            .install_app(&app.path, |_| async {})
            .await
            .map_err(|e| format!("Failed to install app: {e}"))
    }
}

async fn update_profiles(
    app: &RefreshApp,
    device: &Device,
    session: &DeveloperSession,
    team_id: &str,
) -> Result<(), String> {
    let bundle =
        Bundle::new(app.path.clone()).map_err(|e| format!("Failed to create bundle: {e}"))?;
    let options = SignerOptions {
        mode: SignerMode::Pem,
        ..Default::default()
    };
    let mut signer = Signer::new(None, options);
    signer
        .register_bundle(&bundle, session, team_id, true)
        .await
        .map_err(|e| format!("Failed to register bundle: {e}"))?;
    for provision in &signer.provisioning_files {
        device
            .install_profile(provision)
            .await
            .map_err(|e| format!("Failed to install profile: {e}"))?;
    }
    Ok(())
}

fn reschedule(
    store: &AccountStore,
    refresh_device: &RefreshDevice,
    app: &RefreshApp,
) -> Result<(), String> {
    let path = app.path.join("embedded.mobileprovision");
    if !path.exists() {
        return Err("embedded.mobileprovision not found".to_string());
    }
    let provision = MobileProvision::load_with_path(&path)
        .map_err(|e| format!("Failed to load mobile provision: {e}"))?;
    let scheduled_refresh = refresh_time(&provision);

    let mut store = store.clone();
    let mut updated = refresh_device.clone();
    if let Some(existing) = updated.apps.iter_mut().find(|a| a.path == app.path) {
        existing.scheduled_refresh = scheduled_refresh;
    }
    store
        .add_or_update_refresh_device_sync(updated)
        .map_err(|e| format!("Failed to update refresh schedule: {e}"))?;
    log::info!("Next refresh scheduled for: {scheduled_refresh}");
    Ok(())
}

/// SideStore-family apps ship the signing certificate as a p12 next to their executable.
fn embeds_certificate(app: &Path) -> bool {
    ["ALTCertificate.p12", "Certificate.p12"]
        .iter()
        .any(|name| app.join(name).exists())
        || app
            .join("SideStoreApp.framework/ALTCertificate.p12")
            .exists()
}
