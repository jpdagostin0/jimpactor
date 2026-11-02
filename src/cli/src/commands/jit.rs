use anyhow::Result;
use clap::{Args, Subcommand};
use idevice::usbmuxd::UsbmuxdAddr;
use plume_store::{AccountStore, JitPassword, JitServerSettings};
use plume_utils::jit_server::{self, ConnectedDevices, JitServerStatus};
use plume_utils::{
    DeviceEvent, TailnetConfig, set_tailnet_devices, start_tailnet, watch_devices,
    watch_network_devices, watch_tailnet_devices,
};

use crate::{
    commands::{account::get_settings_path, config::Listen, device::select_device},
    get_data_path,
};

#[derive(Debug, Args)]
#[command(arg_required_else_help = true)]
pub struct JitArgs {
    #[arg(short = 'u', long = "udid", value_name = "UDID", global = true)]
    pub udid: Option<String>,
    #[command(subcommand)]
    pub command: JitCommands,
}

#[derive(Debug, Subcommand)]
pub enum JitCommands {
    /// Launch an app with JIT enabled (iOS 17+)
    Launch {
        #[arg(value_name = "BUNDLE_ID")]
        bundle_id: String,
    },
    /// Enable JIT for a running process
    Attach {
        #[arg(value_name = "PID")]
        pid: u64,
    },
    /// Run the JIT server until interrupted, using the saved settings unless overridden
    Serve {
        #[arg(long, value_name = "PORT")]
        port: Option<u16>,
        #[arg(long, value_enum)]
        listen: Option<Listen>,
        /// Require this password instead of the saved one
        #[arg(long, value_name = "PASSWORD")]
        password: Option<String>,
    },
}

/// Keeps `devices` up to date from every transport, as the GUI does.
fn watch_all(devices: ConnectedDevices) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (usb, network, tailnet) = (tx.clone(), tx.clone(), tx);
    let known = get_data_path().join("wifi_devices.plist");
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Could not start device discovery");
        let local = tokio::task::LocalSet::new();
        local.block_on(&runtime, async move {
            tokio::join!(
                watch_devices(UsbmuxdAddr::from_env_var().unwrap_or_default(), usb),
                watch_network_devices(network, known),
                watch_tailnet_devices(tailnet),
            );
        });
    });
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            let Ok(mut devices) = devices.lock() else {
                return;
            };
            match event {
                DeviceEvent::Connected(device) => {
                    log::info!("Connected: {device}");
                    devices.insert(device.udid.clone(), device);
                }
                DeviceEvent::Disconnected(id) => {
                    devices.retain(|_, device| device.device_id != id);
                }
            }
        }
    });
}

async fn serve(mut settings: JitServerSettings) -> Result<()> {
    settings.enabled = true;
    let devices = ConnectedDevices::default();
    watch_all(devices.clone());
    jit_server::apply(&settings, devices, get_data_path().join("ddi"));

    let mut shown = JitServerStatus::Off;
    loop {
        let status = jit_server::status();
        if status != shown {
            match &status {
                JitServerStatus::Listening(address) => println!("Listening on {address}"),
                JitServerStatus::Failed(error) => println!("{error}"),
                _ => {}
            }
            shown = status;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

pub async fn execute(args: JitArgs) -> Result<()> {
    let ddi = get_data_path().join("ddi");
    match args.command {
        JitCommands::Launch { bundle_id } => {
            let pid = select_device(args.udid)
                .await?
                .launch_with_jit(&bundle_id, ddi)
                .await?;
            println!("Enabled JIT for process {pid}");
        }
        JitCommands::Attach { pid } => {
            select_device(args.udid).await?.attach_jit(pid, ddi).await?;
            println!("Enabled JIT for process {pid}");
        }
        JitCommands::Serve {
            port,
            listen,
            password,
        } => {
            let store = AccountStore::load(&Some(get_settings_path())).await?;
            let tailscale = store.tailscale();
            set_tailnet_devices(tailscale.devices.clone());
            if tailscale.enabled {
                start_tailnet(TailnetConfig {
                    hostname: tailscale.hostname.clone(),
                    control_url: tailscale.control_url.clone(),
                    key_file: get_data_path().join("tailscale_keys.json"),
                    auth_key: None,
                });
            }
            let mut settings = store.jit_server().clone();
            if let Some(port) = port {
                settings.port = port;
            }
            if let Some(listen) = listen {
                settings.listen = listen.into();
            }
            if let Some(password) = password {
                settings.password = Some(JitPassword::new(&password));
            }
            serve(settings).await?;
        }
    }
    Ok(())
}
