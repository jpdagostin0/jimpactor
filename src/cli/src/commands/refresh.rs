use anyhow::{Result, anyhow};
use clap::{Args, Subcommand};
use plume_store::AccountStore;

use crate::{
    commands::{account::get_settings_path, device::select_device, device_commands::reset_prompt},
    get_data_path,
};

#[derive(Debug, Args)]
#[command(arg_required_else_help = true)]
pub struct RefreshArgs {
    #[command(subcommand)]
    pub command: RefreshCommands,
}

#[derive(Debug, Subcommand)]
pub enum RefreshCommands {
    /// List apps set up for Auto Refresh
    List,
    /// Refresh an app now
    Now {
        #[arg(value_name = "UDID")]
        udid: String,
        #[arg(value_name = "BUNDLE_ID")]
        bundle_id: String,
    },
    /// Stop refreshing an app and delete its saved copy
    Forget {
        #[arg(value_name = "UDID")]
        udid: String,
        #[arg(value_name = "BUNDLE_ID")]
        bundle_id: String,
    },
}

pub async fn execute(args: RefreshArgs) -> Result<()> {
    let mut store = AccountStore::load(&Some(get_settings_path())).await?;
    match args.command {
        RefreshCommands::List => {
            for (udid, device) in store.refreshes() {
                for app in &device.apps {
                    println!(
                        "{udid}\t{}\t{}\t{}\t{}",
                        device.name,
                        app.bundle_id.as_deref().unwrap_or("???"),
                        app.name.as_deref().unwrap_or("???"),
                        app.scheduled_refresh.format("%Y-%m-%d %H:%M UTC"),
                    );
                }
            }
        }
        RefreshCommands::Now { udid, bundle_id } => {
            let refresh_device = store
                .get_refresh_device(&udid)
                .ok_or_else(|| anyhow!("Nothing is set to refresh on {udid}"))?;
            let app = refresh_device
                .apps
                .iter()
                .find(|app| app.bundle_id.as_deref() == Some(bundle_id.as_str()))
                .ok_or_else(|| anyhow!("{bundle_id} is not set to refresh"))?;
            let device = select_device(Some(udid.clone())).await?;
            plume_utils::refresh::refresh_app(
                &store,
                refresh_device,
                app,
                &device,
                &get_data_path(),
                reset_prompt(),
            )
            .await
            .map_err(anyhow::Error::msg)?;
            println!("Refreshed {bundle_id}");
        }
        RefreshCommands::Forget { udid, bundle_id } => {
            let mut device = store
                .get_refresh_device(&udid)
                .cloned()
                .ok_or_else(|| anyhow!("Nothing is set to refresh on {udid}"))?;
            let matches = |app: &plume_store::RefreshApp| {
                app.bundle_id.as_deref() == Some(bundle_id.as_str())
            };
            for app in device.apps.iter().filter(|app| matches(app)) {
                if app.path.exists() {
                    std::fs::remove_dir_all(&app.path)?;
                }
            }
            device.apps.retain(|app| !matches(app));
            if device.apps.is_empty() {
                store.remove_refresh_device_sync(&udid)?;
            } else {
                store.add_or_update_refresh_device_sync(device)?;
            }
        }
    }
    Ok(())
}
