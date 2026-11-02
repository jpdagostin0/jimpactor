use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::{Result, ensure};
use clap::{Subcommand, ValueEnum};
use dialoguer::Confirm;
use futures::future::BoxFuture;
use plume_store::AccountStore;
use plume_utils::install::{CERTIFICATE_RESET_WARNING, InstallHost};
use plume_utils::refresh::ConfirmReset;
use plume_utils::{
    ArchiveScope, Device, ProfileSort, SignerApp, SignerAppReal, TransferSummary, sort_profiles,
    sources, updates,
};

use crate::{commands::account::get_settings_path, get_data_path};

#[derive(Debug, Subcommand)]
pub enum DeviceCommands {
    /// Show the model, iOS version, battery and storage
    Info,
    /// Restart the device
    Restart,
    /// Shut the device down
    Shutdown,
    /// Pair with the device (accept Trust This Computer on it)
    Trust,
    /// Print the device log until interrupted
    Syslog {
        /// Only print lines containing this text
        #[arg(long, value_name = "TEXT")]
        filter: Option<String>,
    },
    /// Show whether Developer Mode is on
    DeveloperMode {
        /// Show the Developer Mode switch in Settings on the device
        #[arg(long)]
        reveal: bool,
    },
    /// List installed sideloaded apps (--all includes App Store apps)
    Apps {
        #[arg(long)]
        all: bool,
    },
    /// Uninstall an app
    Uninstall {
        #[arg(value_name = "BUNDLE_ID")]
        bundle_id: String,
    },
    /// List apps with a newer version in your sources
    Updates,
    /// Install updates from your sources (all of them without a bundle ID)
    Update {
        #[arg(value_name = "BUNDLE_ID")]
        bundle_id: Option<String>,
    },
    /// Save an app's Documents and Library to a zip that `restore` can read
    Backup {
        #[arg(value_name = "BUNDLE_ID")]
        bundle_id: String,
        #[arg(short = 'o', long, value_name = "PATH")]
        output: PathBuf,
    },
    /// Write a backup into an app, replacing files that are in the backup
    Restore {
        #[arg(value_name = "BUNDLE_ID")]
        bundle_id: String,
        #[arg(value_name = "BACKUP")]
        backup: PathBuf,
    },
    /// Save every file in an app's container to a zip
    DumpFiles {
        #[arg(value_name = "BUNDLE_ID")]
        bundle_id: String,
        #[arg(short = 'o', long, value_name = "PATH")]
        output: PathBuf,
    },
    /// List installed provisioning profiles
    Profiles {
        #[arg(long, value_enum, default_value_t = SortKey::Expiration)]
        sort: SortKey,
        #[arg(long)]
        descending: bool,
    },
    /// Remove provisioning profiles by UUID, or every expired one
    RemoveProfiles {
        #[arg(value_name = "UUID")]
        uuids: Vec<String>,
        #[arg(long)]
        expired: bool,
    },
    /// List crash logs, newest first
    CrashLogs,
    /// Copy one crash log, by the path from `crash-logs`
    CrashLog {
        #[arg(value_name = "NAME")]
        name: String,
        #[arg(short = 'o', long, value_name = "PATH")]
        output: Option<PathBuf>,
    },
    /// Copy every .ips crash log into a folder
    DumpIps {
        #[arg(value_name = "FOLDER")]
        folder: PathBuf,
    },
    /// Save this device's pairing file
    PairingFile {
        #[arg(short = 'o', long, value_name = "PATH")]
        output: PathBuf,
        /// Include a remote pairing (RPPairing) record
        #[arg(long)]
        remote: bool,
    },
    /// Place the pairing file where a supported app reads it
    InstallPairing {
        #[arg(value_name = "BUNDLE_ID")]
        bundle_id: String,
        #[arg(long)]
        remote: bool,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum SortKey {
    Expiration,
    Name,
    AppId,
    Team,
}

impl From<SortKey> for ProfileSort {
    fn from(key: SortKey) -> Self {
        match key {
            SortKey::Expiration => ProfileSort::Expiration,
            SortKey::Name => ProfileSort::Name,
            SortKey::AppId => ProfileSort::AppId,
            SortKey::Team => ProfileSort::Team,
        }
    }
}

/// Reports installs in the log and asks on the terminal.
pub struct CliHost;

impl InstallHost for CliHost {
    fn status(&self, message: String, percent: i32) {
        log::info!("{percent}% {message}");
    }

    fn confirm_certificate_reset(&self) -> bool {
        confirm_reset()
    }

    fn export_path(&self, _suggested_name: String) -> BoxFuture<'_, Option<PathBuf>> {
        Box::pin(async { None })
    }
}

/// Asks before resetting a certificate; scripts without a terminal are never reset.
pub fn confirm_reset() -> bool {
    log::warn!("{CERTIFICATE_RESET_WARNING}");
    std::io::stdin().is_terminal()
        && Confirm::new()
            .with_prompt("Continue?")
            .default(false)
            .interact()
            .unwrap_or(false)
}

pub fn reset_prompt() -> ConfirmReset {
    Arc::new(confirm_reset)
}

pub fn date(time: SystemTime) -> String {
    plist::Date::from(time)
        .to_xml_format()
        .chars()
        .take(10)
        .collect()
}

fn app_name(app: &SignerAppReal) -> String {
    app.installed_name
        .clone()
        .unwrap_or_else(|| app.app.to_string())
}

fn report(summary: &TransferSummary) {
    println!(
        "{} file(s), {:.1} MB, {} skipped",
        summary.files,
        summary.bytes as f64 / 1_048_576.0,
        summary.skipped.len()
    );
    for path in &summary.skipped {
        log::warn!("Skipped {path}");
    }
}

async fn app_version(device: &Device, bundle_id: &str) -> Result<Option<String>> {
    Ok(device
        .installed_apps()
        .await?
        .into_iter()
        .find(|app| app.bundle_id.as_deref() == Some(bundle_id))
        .and_then(|app| app.version))
}

async fn available_updates(device: &Device) -> Result<Vec<updates::AppUpdate>> {
    let installed = device.installed_apps().await?;
    let sources = sources::SourcesStore::load(&get_data_path())
        .sources()
        .to_vec();
    Ok(updates::find_updates(
        &installed,
        &sources::fetch_all(sources).await,
    ))
}

pub async fn execute(command: DeviceCommands, device: Device) -> Result<()> {
    match command {
        DeviceCommands::Info => {
            let details = device.details().await?;
            let line = |label: &str, value: Option<String>| {
                println!("{label:<10}{}", value.unwrap_or_else(|| "unknown".into()))
            };
            line("Name", details.name);
            line("Model", details.product_type);
            line("iOS", details.ios_version);
            line("Build", details.build_version);
            line("Serial", details.serial_number);
            line("Battery", details.battery_level.map(|l| format!("{l}%")));
            line("Charging", details.battery_charging.map(|c| c.to_string()));
            line("Total", details.storage_total.map(|b| b.to_string()));
            line("Free", details.storage_free.map(|b| b.to_string()));
            println!("{:<10}{}", "UDID", device.udid);
        }
        DeviceCommands::Restart => device.restart().await?,
        DeviceCommands::Shutdown => device.shutdown().await?,
        DeviceCommands::Trust => device.pair().await?,
        DeviceCommands::Syslog { filter } => {
            let filter = filter.map(|filter| filter.to_lowercase());
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let stream = tokio::spawn(async move { device.stream_syslog(tx).await });
            while let Some(line) = rx.recv().await {
                if filter
                    .as_ref()
                    .is_none_or(|filter| line.to_lowercase().contains(filter))
                {
                    println!("{line}");
                }
            }
            stream.await??;
        }
        DeviceCommands::DeveloperMode { reveal } => {
            match device.developer_mode_enabled().await {
                Some(enabled) => println!("Developer Mode: {enabled}"),
                None => println!("This device does not report Developer Mode"),
            }
            if reveal {
                device.reveal_developer_mode_option().await?;
            }
        }
        DeviceCommands::Apps { all } => {
            for app in device.installed_apps().await? {
                if all || app.sideloaded {
                    println!(
                        "{}\t{}\t{}\t{}",
                        app.bundle_id.as_deref().unwrap_or("???"),
                        app_name(&app),
                        app.version.as_deref().unwrap_or("-"),
                        app.expiration
                            .map(|date| date.to_xml_format())
                            .unwrap_or_else(|| "-".into()),
                    );
                }
            }
        }
        DeviceCommands::Uninstall { bundle_id } => device.uninstall_app(&bundle_id).await?,
        DeviceCommands::Updates => {
            for update in available_updates(&device).await? {
                println!(
                    "{}\t{}\t{} -> {}",
                    update.bundle_id, update.name, update.installed_version, update.version.version
                );
            }
        }
        DeviceCommands::Update { bundle_id } => {
            let updates: Vec<_> = available_updates(&device)
                .await?
                .into_iter()
                .filter(|update| bundle_id.as_ref().is_none_or(|id| *id == update.bundle_id))
                .collect();
            if updates.is_empty() {
                println!("Nothing to update.");
            }
            let mut store = AccountStore::load(&Some(get_settings_path())).await?;
            let host: Arc<dyn InstallHost> = Arc::new(CliHost);
            for update in updates {
                let account = store.selected_account().cloned();
                plume_utils::install::run_source_install(
                    &update.app,
                    &update.version,
                    Some(&device),
                    account.as_ref(),
                    Some(&mut store),
                    &get_data_path(),
                    &host,
                )
                .await
                .map_err(anyhow::Error::msg)?;
            }
        }
        DeviceCommands::Backup { bundle_id, output } => {
            let version = app_version(&device, &bundle_id).await?;
            let summary = device
                .archive_app_files(&bundle_id, version, ArchiveScope::Backup, &output, |file| {
                    log::info!("{file}")
                })
                .await?;
            report(&summary);
        }
        DeviceCommands::Restore { bundle_id, backup } => {
            let manifest = plume_utils::read_backup_manifest(&backup)?;
            if manifest.bundle_id != bundle_id {
                log::warn!("This backup was made from {}", manifest.bundle_id);
            }
            let summary = device
                .restore_app_files(&bundle_id, &backup, |file| log::info!("{file}"))
                .await?;
            report(&summary);
        }
        DeviceCommands::DumpFiles { bundle_id, output } => {
            let summary = device
                .archive_app_files(
                    &bundle_id,
                    None,
                    ArchiveScope::Everything,
                    &output,
                    |file| log::info!("{file}"),
                )
                .await?;
            report(&summary);
        }
        DeviceCommands::Profiles { sort, descending } => {
            let mut profiles = device.installed_profiles().await?;
            sort_profiles(&mut profiles, sort.into(), descending);
            for profile in profiles {
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    profile.uuid,
                    profile.name,
                    profile.app_id.as_deref().unwrap_or("-"),
                    profile.team_name.as_deref().unwrap_or("-"),
                    profile.expiration.map(date).unwrap_or_else(|| "-".into()),
                );
            }
        }
        DeviceCommands::RemoveProfiles { mut uuids, expired } => {
            if expired {
                uuids.extend(
                    device
                        .installed_profiles()
                        .await?
                        .into_iter()
                        .filter(|profile| profile.is_expired())
                        .map(|profile| profile.uuid),
                );
            }
            ensure!(!uuids.is_empty(), "No profiles to remove");
            println!("Removing {} profile(s)", uuids.len());
            device.remove_profiles(uuids).await?;
        }
        DeviceCommands::CrashLogs => {
            for report in device.crash_reports().await? {
                println!("{report}");
            }
        }
        DeviceCommands::CrashLog { name, output } => {
            let data = device.crash_report(&name).await?;
            let output = output
                .unwrap_or_else(|| PathBuf::from(name.rsplit('/').next().unwrap_or(name.as_str())));
            std::fs::write(&output, data)?;
            println!("Saved {}", output.display());
        }
        DeviceCommands::DumpIps { folder } => {
            std::fs::create_dir_all(&folder)?;
            println!(
                "Saved {} report(s)",
                device.dump_ips_reports(&folder).await?
            );
        }
        DeviceCommands::PairingFile { output, remote } => {
            let data = device.pairing_file(remote, get_data_path()).await?;
            std::fs::write(&output, data)?;
            println!("Saved {}", output.display());
        }
        DeviceCommands::InstallPairing { bundle_id, remote } => {
            let name = device
                .installed_apps()
                .await?
                .into_iter()
                .find(|app| app.bundle_id.as_deref() == Some(bundle_id.as_str()))
                .and_then(|app| app.installed_name);
            let app = SignerApp::from_bundle_identifier_or_name(Some(&bundle_id), name);
            device
                .install_pairing_files(&bundle_id, app, remote, get_data_path())
                .await?;
        }
    }
    Ok(())
}
