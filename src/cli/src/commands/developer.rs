use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use plume_store::{AccountStore, AnisetteSettings, AscKey};
use plume_utils::{DeveloperItem, XkitSigner};

use crate::{
    commands::account::{get_authenticated_account, get_settings_path, select_team},
    get_data_path,
};

#[derive(Debug, Args)]
#[command(arg_required_else_help = true)]
pub struct DeveloperArgs {
    /// Use the selected Apple ID even when an App Store Connect key is saved
    #[arg(long, global = true)]
    pub apple_id: bool,
    /// Developer team for --apple-id
    #[arg(short = 't', long = "team", value_name = "TEAM_ID", global = true)]
    pub team_id: Option<String>,
    #[command(subcommand)]
    pub command: DeveloperCommands,
}

#[derive(Debug, Subcommand)]
pub enum DeveloperCommands {
    /// Save an App Store Connect API key for `sign --asc-key`
    SetKey(SetKeyArgs),
    /// Remove the saved App Store Connect API key
    RemoveKey,
    /// Show the saved App Store Connect API key's IDs
    ShowKey,
    /// List registered devices
    Devices,
    /// Register a device
    RegisterDevice(RegisterArgs),
    /// Enable a registered device, by the ID from `devices`
    EnableDevice(ItemArgs),
    /// Disable a registered device, by the ID from `devices`
    DisableDevice(ItemArgs),
    /// List signing certificates
    Certificates,
    /// Revoke a signing certificate, by the ID from `certificates`
    RevokeCertificate(ItemArgs),
    /// List bundle IDs
    BundleIds,
    /// Delete a bundle ID, by the ID from `bundle-ids`
    DeleteBundleId(ItemArgs),
    /// List provisioning profiles
    Profiles,
    /// Delete a provisioning profile, by the ID from `profiles`
    DeleteProfile(ItemArgs),
}

#[derive(Debug, Args)]
pub struct SetKeyArgs {
    /// Key ID shown beside the key in App Store Connect
    #[arg(long, value_name = "KEY_ID")]
    pub key_id: String,
    /// Issuer ID shown above the key list in App Store Connect
    #[arg(long, value_name = "ISSUER_ID")]
    pub issuer_id: String,
    /// The AuthKey_*.p8 file downloaded when the key was created
    #[arg(long, value_name = "PATH")]
    pub p8: PathBuf,
}

#[derive(Debug, Args)]
pub struct RegisterArgs {
    #[arg(short = 'u', long = "udid", value_name = "UDID")]
    pub udid: String,
    #[arg(short = 'n', long = "name", value_name = "NAME")]
    pub name: String,
}

#[derive(Debug, Args)]
pub struct ItemArgs {
    #[arg(value_name = "ID")]
    pub id: String,
}

fn print_items(items: Vec<DeveloperItem>) {
    for item in items {
        println!("{}\t{}\t{}", item.id, item.name, item.detail);
    }
}

async fn store() -> Result<AccountStore> {
    Ok(AccountStore::load(&Some(get_settings_path())).await?)
}

/// The saved key's client, or the selected Apple ID's when there is no key.
async fn client(args: &DeveloperArgs) -> Result<XkitSigner> {
    let store = store().await?;
    if !args.apple_id
        && let Some(key) = store.asc_key()
    {
        return Ok(XkitSigner::with_asc_key(key, &get_data_path())?);
    }

    let session = get_authenticated_account().await?;
    let team_id = select_team(&session, args.team_id.as_deref()).await?;
    Ok(XkitSigner::new(
        session.adsid(),
        session.xcode_gs_token(),
        AnisetteSettings::load_configuration(&get_data_path()),
        &team_id,
        &get_data_path(),
    )
    .await?)
}

pub async fn execute(args: DeveloperArgs) -> Result<()> {
    match &args.command {
        DeveloperCommands::SetKey(key) => {
            let pem = std::fs::read_to_string(&key.p8)
                .with_context(|| format!("Could not read {}", key.p8.display()))?;
            let key = AscKey {
                key_id: key.key_id.clone(),
                issuer_id: key.issuer_id.clone(),
                pem,
            };
            XkitSigner::with_asc_key(&key, &get_data_path())?;
            store().await?.set_asc_key_sync(Some(key))?;
            println!("Saved. Sign with: jimpactor-cli sign --asc-key ...");
        }
        DeveloperCommands::RemoveKey => {
            store().await?.set_asc_key_sync(None)?;
            println!("Removed.");
        }
        DeveloperCommands::ShowKey => match store().await?.asc_key() {
            Some(key) => println!("Key {} (issuer {})", key.key_id, key.issuer_id),
            None => println!("No App Store Connect key is saved."),
        },
        DeveloperCommands::Devices => {
            for device in client(&args).await?.registered_devices().await? {
                println!(
                    "{}\t{}\t{}\t{}",
                    device.id, device.name, device.status, device.udid
                );
            }
        }
        DeveloperCommands::RegisterDevice(device) => {
            client(&args)
                .await?
                .register_device(&device.udid, &device.name)
                .await?;
            println!("Registered {}.", device.udid);
        }
        DeveloperCommands::EnableDevice(item) => {
            client(&args)
                .await?
                .set_device_enabled(&item.id, true)
                .await?;
        }
        DeveloperCommands::DisableDevice(item) => {
            client(&args)
                .await?
                .set_device_enabled(&item.id, false)
                .await?;
        }
        DeveloperCommands::Certificates => {
            print_items(client(&args).await?.certificates().await?);
        }
        DeveloperCommands::RevokeCertificate(item) => {
            client(&args).await?.revoke_certificate(&item.id).await?;
        }
        DeveloperCommands::BundleIds => print_items(client(&args).await?.bundle_ids().await?),
        DeveloperCommands::DeleteBundleId(item) => {
            client(&args).await?.delete_bundle_id(&item.id).await?;
        }
        DeveloperCommands::Profiles => print_items(client(&args).await?.profiles().await?),
        DeveloperCommands::DeleteProfile(item) => {
            client(&args).await?.delete_profile(&item.id).await?;
        }
    }
    Ok(())
}
