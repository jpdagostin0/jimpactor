use std::path::PathBuf;

use anyhow::{Result, anyhow};
use clap::{Args, Subcommand};
use plume_utils::ipc::{self, Event, Request};

use crate::get_data_path;

#[derive(Debug, Args)]
#[command(arg_required_else_help = true)]
pub struct TrayArgs {
    #[command(subcommand)]
    pub command: TrayCommands,
}

#[derive(Debug, Subcommand)]
pub enum TrayCommands {
    /// List the devices the running Jimpactor can see
    Devices,
    /// Have the running Jimpactor sign and install an IPA
    Install {
        #[arg(value_name = "IPA")]
        path: PathBuf,
        /// Target device (default: the locked device, or the only connected one)
        #[arg(long)]
        udid: Option<String>,
    },
    /// Print the IPC protocol's JSON Schema
    Schema,
}

pub async fn execute(args: TrayArgs) -> Result<()> {
    let request = match args.command {
        TrayCommands::Schema => {
            print!("{}", ipc::SCHEMA);
            return Ok(());
        }
        TrayCommands::Devices => Request::ListDevices,
        TrayCommands::Install { path, udid } => Request::Install {
            path: std::path::absolute(&path).map_err(|e| anyhow!("{}: {e}", path.display()))?,
            udid,
        },
    };
    // The client blocks on its socket for the whole install.
    let event = tokio::task::spawn_blocking(move || {
        ipc::send(&get_data_path(), request, |message, percent| {
            eprintln!("[{percent:>3}%] {message}");
        })
    })
    .await?
    .map_err(anyhow::Error::msg)?;

    match event {
        Event::Devices { devices } if devices.is_empty() => println!("No devices connected."),
        Event::Devices { devices } => {
            for device in devices {
                let state = if device.available {
                    ""
                } else {
                    " (unreachable)"
                };
                println!(
                    "{}  {} [{}]{state}",
                    device.udid, device.name, device.connection
                );
            }
        }
        Event::Done { message } => println!("{message}"),
        other => println!("{}", serde_json::to_string(&other)?),
    }
    Ok(())
}
