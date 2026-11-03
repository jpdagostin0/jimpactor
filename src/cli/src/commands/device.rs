use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::{Error, Ok, Result};
use clap::Args;
use dialoguer::Select;
use idevice::{IdeviceService, installation_proxy::InstallationProxyClient, usbmuxd::UsbmuxdAddr};
use plume_utils::{Device, DeviceEvent, Package, get_device_for_id, watch_network_devices};

use crate::{commands::device_commands::DeviceCommands, get_data_path};

#[derive(Debug, Args)]
#[command(arg_required_else_help = true)]
pub struct DeviceArgs {
    #[command(subcommand)]
    pub command: Option<DeviceCommands>,
    /// Device UDID to target (optional, will prompt if not provided)
    #[arg(short = 'u', long = "udid", value_name = "UDID", global = true)]
    pub udid: Option<String>,
    /// Install app at specified path to device (.ipa, .app)
    #[arg(short = 'i', long = "install", value_name = "PATH")]
    pub install: Option<PathBuf>,
    /// Install pairing record from specified path to device
    #[arg(
        short = 'p',
        long = "pairing",
        value_name = "MAC",
        requires = "pairing_path"
    )]
    pub pairing: bool,
    /// Path to pairing record to install (i.e. /Documents/pairingFile.plist)
    #[arg(long = "pairing-path", value_name = "PATH", requires = "pairing")]
    pub pairing_path: Option<PathBuf>,
    /// App identifier for the app to use for pairing record installation (optional, will prompt if not provided)
    #[arg(long = "pairing-app-identifier", value_name = "IDENTIFIER")]
    pub pairing_app_identifier: Option<String>,
    /// App whose container to browse with --ls / --pull (optional, will prompt if not provided)
    #[arg(short = 'a', long = "app", value_name = "IDENTIFIER")]
    pub app: Option<String>,
    /// List a folder inside the app's container (default: /Documents)
    #[arg(long = "ls", value_name = "DIR", num_args = 0..=1, default_missing_value = "/Documents")]
    pub ls: Option<String>,
    /// Copy a file out of the app's container (i.e. /Documents/output.json)
    #[arg(long = "pull", value_name = "PATH")]
    pub pull: Option<String>,
    /// Where to save the file from --pull (default: its file name in the current folder)
    #[arg(short = 'o', long = "output", value_name = "PATH", requires = "pull")]
    pub output: Option<PathBuf>,
    /// Install to connected Mac (arm64 only)
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[arg(short = 'm', long = "mac", value_name = "MAC", conflicts_with_all = ["udid", "pairing", "ls", "pull"])]
    pub mac: bool,
}

pub async fn execute(args: DeviceArgs) -> Result<()> {
    let device = {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            if args.mac {
                Device {
                    name: "My Mac".to_string(),
                    udid: String::new(),
                    device_id: 0,
                    usbmuxd_device: None,
                    usbmuxd_address: None,
                    network: None,
                    is_mac: true,
                }
            } else {
                select_device(args.udid).await?
            }
        }
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        {
            select_device(args.udid).await?
        }
    };

    if let Some(command) = args.command {
        return crate::commands::device_commands::execute(command, device).await;
    }

    if let Some(app_path) = args.install {
        let mut app_path = app_path;

        if !app_path.is_dir() {
            app_path = Package::new(app_path)?
                .get_package_bundle()?
                .bundle_dir()
                .clone();
        }

        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        if args.mac {
            log::info!("Installing app at {:?} to connected Mac", app_path);
            plume_utils::install_app_mac(&app_path).await?;
            return Ok(());
        }

        log::info!("Installing app at {:?} to device {}", app_path, device.name);
        device
            .install_app(&app_path, |progress| async move {
                log::info!("{}", progress);
            })
            .await?;
    }

    if args.pairing
        && let Some(pairing_path) = args.pairing_path
    {
        log::info!(
            "Installing pairing record from {:?} to device {}",
            pairing_path,
            device.name
        );
        let app_identifier = if let Some(identifier) = args.pairing_app_identifier {
            identifier
        } else {
            apps(&device).await?
        };
        device
            .install_pairing_record(&app_identifier, pairing_path.to_str().unwrap())
            .await?;
    }

    if args.ls.is_some() || args.pull.is_some() {
        let app_identifier = match args.app {
            Some(identifier) => identifier,
            None => apps(&device).await?,
        };

        if let Some(dir) = args.ls {
            for entry in device.list_app_documents(&app_identifier, &dir).await? {
                println!("{entry}");
            }
        }

        if let Some(remote_path) = args.pull {
            let data = device
                .read_app_document(&app_identifier, &remote_path)
                .await?;
            let output = match args.output {
                Some(output) if output.is_dir() => output.join(remote_file_name(&remote_path)?),
                Some(output) => output,
                None => PathBuf::from(remote_file_name(&remote_path)?),
            };
            std::fs::write(&output, &data)?;
            log::info!(
                "Saved {remote_path} from {app_identifier} to {:?} ({} bytes)",
                output,
                data.len()
            );
        }
    }

    Ok(())
}

fn remote_file_name(path: &str) -> Result<&str> {
    path.rsplit('/')
        .find(|name| !name.is_empty())
        .ok_or_else(|| anyhow::anyhow!("{path} does not name a file"))
}

/// Looks for paired devices on Wi-Fi. Their connections live on the discovery thread, which
/// is left running for as long as the command needs them.
async fn discover_network_devices(wait: std::time::Duration) -> Vec<Device> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let known = get_data_path().join("wifi_devices.plist");
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Could not start Wi-Fi discovery");
        runtime.block_on(watch_network_devices(tx, known));
    });

    let mut found = Vec::new();
    let _ = tokio::time::timeout(wait, async {
        while let Some(event) = rx.recv().await {
            if let DeviceEvent::Connected(device) = event {
                found.push(device);
            }
        }
    })
    .await;
    found
}

pub async fn select_device(device_udid: Option<String>) -> Result<Device> {
    const WIFI_WAIT: std::time::Duration = std::time::Duration::from_secs(6);

    if let Some(udid) = device_udid {
        return match get_device_for_id(&udid).await {
            std::result::Result::Ok(device) => Ok(device),
            Err(error) => discover_network_devices(WIFI_WAIT)
                .await
                .into_iter()
                .find(|device| device.udid == udid)
                .ok_or_else(|| error.into()),
        };
    }

    // usbmuxd may be missing entirely on a machine that only uses Wi-Fi.
    let devices = match UsbmuxdAddr::from_env_var()
        .unwrap_or_default()
        .connect(0)
        .await
    {
        std::result::Result::Ok(mut muxer) => muxer.get_devices().await.unwrap_or_default(),
        Err(_) => Vec::new(),
    };

    let device_futures: Vec<_> = devices.into_iter().map(Device::new).collect();

    let mut devices = futures::future::join_all(device_futures).await;
    if devices.is_empty() {
        devices = discover_network_devices(WIFI_WAIT).await;
    }

    if devices.is_empty() {
        return Err(anyhow::anyhow!(
            "No devices connected. Please connect a device or specify a UDID with --udid"
        ));
    }

    if devices.len() == 1 {
        return Ok(devices.remove(0));
    }

    let device_names: Vec<String> = devices.iter().map(|d| d.to_string()).collect();
    if !std::io::stdin().is_terminal() {
        let udids: Vec<&str> = devices.iter().map(|d| d.udid.as_str()).collect();
        return Err(anyhow::anyhow!(
            "Several devices are connected. Choose one with --udid: {}",
            udids.join(", ")
        ));
    }

    let selection = Select::new()
        .with_prompt("Select a device to register and install to")
        .items(&device_names)
        .default(0)
        .interact()?;

    Ok(devices[selection].clone())
}

async fn apps(device: &Device) -> Result<String, Error> {
    const INSTALLATION_LABEL: &str = "App Installation";
    let p = device.provider(INSTALLATION_LABEL)?;

    let mut lpc = InstallationProxyClient::connect(p.as_ref())
        .await
        .map_err(|e| anyhow::anyhow!("Failed to create installation proxy client: {}", e))?;

    let ia = lpc
        .get_apps(Some("User"), None)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to get installed apps: {}", e))?;

    let app_names: Vec<String> = ia.keys().cloned().collect();

    let selection = Select::new()
        .items(&app_names)
        .default(0)
        .with_prompt("Select an installed app")
        .interact()?;

    Ok(app_names[selection].clone())
}
