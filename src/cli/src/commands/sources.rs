use std::sync::Arc;

use anyhow::{Result, anyhow};
use clap::{Args, Subcommand};
use plume_store::AccountStore;
use plume_utils::install::{InstallHost, run_source_install};
use plume_utils::sources::{self, SourcesStore};

use crate::{
    commands::{account::get_settings_path, device::select_device, device_commands::CliHost},
    get_data_path,
};

#[derive(Debug, Args)]
#[command(arg_required_else_help = true)]
pub struct SourcesArgs {
    #[command(subcommand)]
    pub command: SourcesCommands,
}

#[derive(Debug, Subcommand)]
pub enum SourcesCommands {
    /// List your sources
    List,
    /// Add a GitHub repository or AltStore source
    Add {
        #[arg(value_name = "URL")]
        url: String,
    },
    /// Remove a source, by its URL
    Remove {
        #[arg(value_name = "URL")]
        url: String,
    },
    /// List the apps your sources offer
    Apps,
    /// Download and install an app from your sources
    Install {
        #[arg(value_name = "APP")]
        name: String,
        /// Version to install (default: the newest stable one)
        #[arg(long, value_name = "VERSION")]
        version: Option<String>,
        #[arg(short = 'u', long = "udid", value_name = "UDID")]
        udid: Option<String>,
    },
}

async fn apps() -> Vec<sources::SourceApp> {
    let store = SourcesStore::load(&get_data_path());
    sources::fetch_all(store.sources().to_vec()).await
}

pub async fn execute(args: SourcesArgs) -> Result<()> {
    let data_path = get_data_path();
    match args.command {
        SourcesCommands::List => {
            for source in SourcesStore::load(&data_path).sources() {
                println!("{}\t{}\t{}", source.name, source.kind, source.url);
            }
        }
        SourcesCommands::Add { url } => {
            let source = sources::identify(&url).await.map_err(anyhow::Error::msg)?;
            let mut store = SourcesStore::load(&data_path);
            println!("Added {}", source.name);
            store.add(source).map_err(anyhow::Error::msg)?;
            store.save(&data_path).map_err(anyhow::Error::msg)?;
        }
        SourcesCommands::Remove { url } => {
            let mut store = SourcesStore::load(&data_path);
            store.remove(&url);
            store.save(&data_path).map_err(anyhow::Error::msg)?;
        }
        SourcesCommands::Apps => {
            for app in apps().await {
                let version = app
                    .default_version()
                    .map_or("-".to_owned(), |version| version.to_string());
                println!("{app}\t{version}");
            }
        }
        SourcesCommands::Install {
            name,
            version,
            udid,
        } => {
            let apps = apps().await;
            let app = apps
                .iter()
                .find(|app| app.name.eq_ignore_ascii_case(&name))
                .ok_or_else(|| anyhow!("No source offers {name}"))?;
            let chosen = match &version {
                Some(version) => app.versions.iter().find(|v| &v.version == version),
                None => app.default_version(),
            }
            .ok_or_else(|| anyhow!("{name} has no such version"))?;

            let device = select_device(udid).await?;
            let mut store = AccountStore::load(&Some(get_settings_path())).await?;
            let account = store.selected_account().cloned();
            let host: Arc<dyn InstallHost> = Arc::new(CliHost);
            run_source_install(
                app,
                chosen,
                Some(&device),
                account.as_ref(),
                Some(&mut store),
                &data_path,
                &host,
            )
            .await
            .map_err(anyhow::Error::msg)?;
        }
    }
    Ok(())
}
