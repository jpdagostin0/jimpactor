mod commands;

use std::path::PathBuf;

use clap::Parser;
use commands::{Cli, Commands};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let _ = rustls::crypto::ring::default_provider().install_default();
    let cli = Cli::parse();

    match cli.command {
        Commands::Sign(args) => commands::sign::execute(*args).await?,
        Commands::MachO(args) => commands::macho::execute(args).await?,
        Commands::Account(args) => commands::account::execute(args).await?,
        Commands::Device(args) => commands::device::execute(args).await?,
        Commands::Developer(args) => commands::developer::execute(args).await?,
        Commands::Sources(args) => commands::sources::execute(args).await?,
        Commands::Anisette(args) => commands::anisette::execute(args).await?,
        Commands::Config(args) => commands::config::execute(args).await?,
        Commands::Refresh(args) => commands::refresh::execute(args).await?,
        Commands::Jit(args) => commands::jit::execute(args).await?,
        Commands::AppProfile(args) => commands::app_profile::execute(args)?,
        Commands::Ipa(args) => commands::ipa::execute(args)?,
        Commands::CheckUpdate => {
            let current = env!("CARGO_PKG_VERSION");
            match plume_utils::sources::check_for_update(current)
                .await
                .map_err(anyhow::Error::msg)?
            {
                Some(release) => println!("{} is available: {}", release.version, release.url),
                None => println!("{current} is up to date."),
            }
        }
    }

    Ok(())
}

pub fn get_data_path() -> PathBuf {
    plume_store::data_path()
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
    fn asc_key_signing_excludes_other_identities() {
        let args = ["jimpactor-cli", "sign", "--package", "App.ipa", "--asc-key"];
        assert!(matches!(
            Cli::try_parse_from(args).unwrap().command,
            Commands::Sign(args) if args.asc_key
        ));
        for other in ["--apple-id", "--modify-only"] {
            let mut args = args.to_vec();
            args.push(other);
            assert!(Cli::try_parse_from(args).is_err());
        }
    }
    #[test]
    fn every_gui_area_has_a_command() {
        for args in [
            &["device", "--udid", "x", "info"][..],
            &["device", "backup", "a.b", "-o", "a.zip"],
            &["device", "restore", "a.b", "a.zip"],
            &["device", "dump-files", "a.b", "-o", "a.zip"],
            &["device", "profiles", "--sort", "app-id", "--descending"],
            &["device", "remove-profiles", "--expired"],
            &["device", "dump-ips", "folder"],
            &["device", "update"],
            &["sources", "install", "SideStore", "--version", "1.0"],
            &["anisette", "use", "https://example.com"],
            &["config", "set", "refresh-alert-days", "3"],
            &["config", "jit", "--port", "9000", "--listen", "tailnet"],
            &["refresh", "now", "udid", "a.b"],
            &["jit", "--udid", "x", "launch", "a.b"],
            &["jit", "serve", "--port", "9000"],
            &["app-profile", "export", "a.b", "out.json"],
            &["account", "teams"],
            &["ipa", "App.ipa", "--json"],
            &["check-update"],
            &["account", "set-team", "TEAM"],
            &["account", "export-p12", "-o", "a.p12"],
        ] {
            let args = std::iter::once(&"jimpactor-cli").chain(args);
            if let Err(error) = Cli::try_parse_from(args) {
                panic!("{error}");
            }
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
