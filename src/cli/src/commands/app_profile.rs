use std::path::PathBuf;

use anyhow::{Result, anyhow};
use clap::{Args, Subcommand};
use plume_utils::{AppProfile, AppProfiles};

use crate::get_data_path;

#[derive(Debug, Args)]
#[command(arg_required_else_help = true)]
pub struct AppProfileArgs {
    #[command(subcommand)]
    pub command: AppProfileCommands,
}

#[derive(Debug, Subcommand)]
pub enum AppProfileCommands {
    /// List apps with saved options
    List,
    /// Print an app's saved options
    Show {
        #[arg(value_name = "BUNDLE_ID")]
        bundle_id: String,
    },
    /// Delete an app's saved options
    Delete {
        #[arg(value_name = "BUNDLE_ID")]
        bundle_id: String,
    },
    /// Copy an app's saved options to a file
    Export {
        #[arg(value_name = "BUNDLE_ID")]
        bundle_id: String,
        #[arg(value_name = "PATH")]
        path: PathBuf,
    },
    /// Save options from a file
    Import {
        #[arg(value_name = "PATH")]
        path: PathBuf,
    },
}

pub fn execute(args: AppProfileArgs) -> Result<()> {
    let profiles = AppProfiles::new(&get_data_path());
    let find = |bundle_id: &str| {
        profiles
            .load(bundle_id)
            .ok_or_else(|| anyhow!("Nothing is saved for {bundle_id}"))
    };
    match args.command {
        AppProfileCommands::List => {
            for profile in profiles.list() {
                println!("{}", profile.bundle_id);
            }
        }
        AppProfileCommands::Show { bundle_id } => {
            println!("{:#?}", find(&bundle_id)?.options);
        }
        AppProfileCommands::Delete { bundle_id } => profiles.delete(&bundle_id)?,
        AppProfileCommands::Export { bundle_id, path } => find(&bundle_id)?.write(&path)?,
        AppProfileCommands::Import { path } => profiles.save(&AppProfile::read(&path)?)?,
    }
    Ok(())
}
