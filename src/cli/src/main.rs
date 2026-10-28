mod commands;

use std::{
    env, fs,
    path::{Path, PathBuf},
};

use clap::Parser;
use commands::{Cli, Commands};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("debug")).init();
    let _ = rustls::crypto::ring::default_provider().install_default();
    let cli = Cli::parse();

    match cli.command {
        Commands::Sign(args) => commands::sign::execute(*args).await?,
        Commands::MachO(args) => commands::macho::execute(args).await?,
        Commands::Account(args) => commands::account::execute(args).await?,
        Commands::Device(args) => commands::device::execute(args).await?,
    }

    Ok(())
}

pub fn get_data_path() -> PathBuf {
    let base = if cfg!(windows) {
        env::var("APPDATA").unwrap()
    } else {
        env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| env::var("HOME").unwrap() + "/.config")
    };

    let dir = Path::new(&base).join("Jimpactor");

    fs::create_dir_all(&dir).ok();

    dir
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    #[test]
    fn modification_only_is_explicit_and_cannot_install_or_use_an_account() {
        let cli = Cli::try_parse_from([
            "jimpactor-cli",
            "sign",
            "--package",
            "App.ipa",
            "--output",
            "Modified.ipa",
            "--modify-only",
            "--url-scheme",
            "duplicate-app",
            "--tweak-loader",
            "none",
        ])
        .unwrap();
        assert!(
            matches!(cli.command, Commands::Sign(args) if args.modify_only && args.url_schemes == ["duplicate-app"] && args.tweak_loader == "none")
        );
        for incompatible in ["--apple-id", "--register-and-install"] {
            assert!(
                Cli::try_parse_from([
                    "jimpactor-cli",
                    "sign",
                    "--package",
                    "App.ipa",
                    "--modify-only",
                    incompatible
                ])
                .is_err()
            );
        }
    }
    #[test]
    fn command_schema_and_device_udid_are_valid_on_this_platform() {
        Cli::command().debug_assert();
        let cli =
            Cli::try_parse_from(["jimpactor-cli", "device", "--udid", "test-device"]).unwrap();
        assert!(
            matches!(cli.command, Commands::Device(args) if args.udid.as_deref() == Some("test-device"))
        );
    }
}
