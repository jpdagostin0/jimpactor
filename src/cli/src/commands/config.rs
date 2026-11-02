use anyhow::{Result, anyhow, bail};
use clap::{Args, Subcommand, ValueEnum};
use plume_store::{AccountStore, AppearanceMode, JitListen, TailscaleSettings};

use crate::commands::account::get_settings_path;

#[derive(Debug, Args)]
#[command(arg_required_else_help = true)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: ConfigCommands,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Setting {
    Keyring,
    RemotePairing,
    RefreshAlerts,
    RefreshAlertDays,
    LockDevice,
    CloseToTray,
    Appearance,
    Locale,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Listen {
    Local,
    Network,
    Tailnet,
}

impl From<Listen> for JitListen {
    fn from(listen: Listen) -> Self {
        match listen {
            Listen::Local => JitListen::Local,
            Listen::Network => JitListen::Network,
            Listen::Tailnet => JitListen::Tailnet,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommands {
    /// Show every setting
    Show,
    /// Change a setting
    Set {
        #[arg(value_enum)]
        setting: Setting,
        /// on/off, a number, system/light/dark, or a language code (`system` to follow the OS)
        #[arg(value_name = "VALUE")]
        value: String,
    },
    /// Change the Tailscale settings
    Tailscale {
        #[arg(long, value_name = "true|false")]
        enabled: Option<bool>,
        #[arg(long, value_name = "NAME")]
        hostname: Option<String>,
        /// A custom coordination server; empty to use Tailscale's
        #[arg(long, value_name = "URL")]
        control_url: Option<String>,
        /// Add a device machine name or tailnet address
        #[arg(long, value_name = "DEVICE")]
        add_device: Vec<String>,
        #[arg(long, value_name = "DEVICE")]
        remove_device: Vec<String>,
    },
    /// Change the JIT server settings
    Jit {
        #[arg(long, value_name = "true|false")]
        enabled: Option<bool>,
        #[arg(long, value_name = "PORT")]
        port: Option<u16>,
        #[arg(long, value_enum)]
        listen: Option<Listen>,
    },
}

fn switch(value: &str) -> Result<bool> {
    match value {
        "on" | "true" | "yes" => Ok(true),
        "off" | "false" | "no" => Ok(false),
        _ => bail!("Expected on or off, not {value}"),
    }
}

pub fn edit_tailscale(settings: &mut TailscaleSettings, add: Vec<String>, remove: &[String]) {
    settings.devices.retain(|device| !remove.contains(device));
    for device in add {
        if !settings.devices.contains(&device) {
            settings.devices.push(device);
        }
    }
}

pub async fn execute(args: ConfigArgs) -> Result<()> {
    let mut store = AccountStore::load(&Some(get_settings_path())).await?;
    match args.command {
        ConfigCommands::Show => {
            println!("keyring:            {}", store.use_keyring());
            println!("remote-pairing:     {}", store.remote_pairing());
            println!("refresh-alerts:     {}", store.refresh_alerts());
            println!("refresh-alert-days: {}", store.refresh_alert_days());
            println!("lock-device:        {}", store.lock_device());
            println!("close-to-tray:      {}", store.close_to_tray());
            println!("appearance:         {:?}", store.appearance());
            println!("locale:             {}", store.locale().unwrap_or("system"));
            println!("tailscale:          {:?}", store.tailscale());
            println!("jit:                {:?}", store.jit_server());
        }
        ConfigCommands::Set { setting, value } => match setting {
            Setting::Keyring => store.set_use_keyring_sync(switch(&value)?)?,
            Setting::RemotePairing => store.set_remote_pairing_sync(switch(&value)?)?,
            Setting::RefreshAlerts => store.set_refresh_alerts_sync(switch(&value)?)?,
            Setting::RefreshAlertDays => store.set_refresh_alert_days_sync(value.parse()?)?,
            Setting::LockDevice => store.set_lock_device_sync(switch(&value)?)?,
            Setting::CloseToTray => store.set_close_to_tray_sync(switch(&value)?)?,
            Setting::Appearance => store.set_appearance_sync(match value.as_str() {
                "system" => AppearanceMode::System,
                "light" => AppearanceMode::Light,
                "dark" => AppearanceMode::Dark,
                _ => return Err(anyhow!("Expected system, light or dark")),
            })?,
            Setting::Locale => {
                store.set_locale_sync((value != "system").then_some(value))?;
            }
        },
        ConfigCommands::Tailscale {
            enabled,
            hostname,
            control_url,
            add_device,
            remove_device,
        } => {
            let mut settings = store.tailscale().clone();
            if let Some(enabled) = enabled {
                settings.enabled = enabled;
            }
            if let Some(hostname) = hostname {
                settings.hostname = hostname;
            }
            if let Some(url) = control_url {
                settings.control_url = (!url.is_empty()).then_some(url);
            }
            edit_tailscale(&mut settings, add_device, &remove_device);
            store.set_tailscale_sync(settings)?;
        }
        ConfigCommands::Jit {
            enabled,
            port,
            listen,
        } => {
            let mut settings = store.jit_server().clone();
            if let Some(enabled) = enabled {
                settings.enabled = enabled;
            }
            if let Some(port) = port {
                settings.port = port;
            }
            if let Some(listen) = listen {
                settings.listen = listen.into();
            }
            store.set_jit_server_sync(settings)?;
        }
    }
    Ok(())
}
