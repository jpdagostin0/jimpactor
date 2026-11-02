use clap::{Parser, Subcommand};

pub mod account;
pub mod anisette;
pub mod app_profile;
pub mod config;
pub mod developer;
pub mod device;
pub mod device_commands;
pub mod jit;
pub mod macho;
pub mod refresh;
pub mod sign;
pub mod sources;

#[derive(Debug, Parser)]
#[command(
    name = "jimpactor-cli",
    author,
    version,
    about = "iOS code signing and inspection tool",
    disable_help_subcommand = true,
    arg_required_else_help = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Sign an iOS app bundle with certificate and provisioning profile
    Sign(Box<sign::SignArgs>),
    /// Inspect Mach-O binaries
    MachO(macho::MachArgs),
    /// Manage Apple Developer account authentication
    Account(account::AccountArgs),
    /// Device management commands
    Device(device::DeviceArgs),
    /// Manage developer resources and App Store Connect API keys
    Developer(developer::DeveloperArgs),
    /// Manage app sources and install from them
    Sources(sources::SourcesArgs),
    /// Choose and test the anisette provider
    Anisette(anisette::AnisetteArgs),
    /// View and change settings
    Config(config::ConfigArgs),
    /// Manage Auto Refresh
    Refresh(refresh::RefreshArgs),
    /// Enable JIT or run the JIT server
    Jit(jit::JitArgs),
    /// Manage saved per-app install options
    AppProfile(app_profile::AppProfileArgs),
}
