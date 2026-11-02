use anyhow::Result;
use clap::{Args, Subcommand, ValueEnum};
use plume_core::auth::anisette_data::AnisetteData;
use plume_core::remote_servers::{self, AnisetteServer};
use plume_store::{
    AnisetteMode, AnisetteSettings, RemoteProvider, SideloadlyAccount, SideloadlyAuth,
    SideloadlySession,
};

use crate::get_data_path;

#[derive(Debug, Args)]
#[command(arg_required_else_help = true)]
pub struct AnisetteArgs {
    #[command(subcommand)]
    pub command: AnisetteCommands,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Mode {
    Local,
    Remote,
    Fake,
}

#[derive(Debug, Subcommand)]
pub enum AnisetteCommands {
    /// Show the current provider
    Show,
    /// Choose where anisette data comes from
    Mode {
        #[arg(value_enum)]
        mode: Mode,
    },
    /// Fetch anisette data once and report how long it took
    Test,
    /// Forget this computer's anisette identity
    Reset,
    /// List known remote servers
    Servers,
    /// Use a remote server, by its URL
    Use {
        #[arg(value_name = "URL")]
        url: String,
    },
    /// Find the fastest public server and use it
    Fastest,
    /// Sign in to Sideloadly's anisette service
    SideloadlyLogin {
        /// Use a Patreon account instead of signing in anonymously
        #[arg(long)]
        patreon: bool,
    },
    /// Sign out of Sideloadly's anisette service
    SideloadlyLogout,
    /// Check the Sideloadly connection
    SideloadlyStatus,
    /// Get a new Sideloadly device token
    SideloadlyRefresh,
}

fn use_server(settings: &mut AnisetteSettings, server: AnisetteServer, provider: RemoteProvider) {
    println!("Using {server}");
    settings.remote_server = server;
    settings.remote_provider = provider;
    settings.mode = AnisetteMode::Remote;
}

pub async fn execute(args: AnisetteArgs) -> Result<()> {
    let path = get_data_path();
    let mut settings = AnisetteSettings::load_sync(&path);
    match args.command {
        AnisetteCommands::Show => {
            println!("Mode:   {:?}", settings.mode);
            // Sideloadly URLs carry a private token, so only the host is shown.
            let address = &settings.remote_server.address;
            let host = address
                .split("://")
                .nth(1)
                .and_then(|rest| rest.split(['/', '?']).next())
                .unwrap_or(address);
            println!("Server: {} ({host})", settings.remote_server.name);
            if let Some(session) = &settings.sideloadly_session {
                println!("Sideloadly: {:?}", session.account);
            }
            return Ok(());
        }
        AnisetteCommands::Mode { mode } => {
            settings.mode = match mode {
                Mode::Local => AnisetteMode::Local,
                Mode::Remote => AnisetteMode::Remote,
                Mode::Fake => AnisetteMode::Fake,
            };
        }
        AnisetteCommands::Test => {
            let start = std::time::Instant::now();
            AnisetteData::new(settings.configuration(&path)).await?;
            println!("OK in {} ms", start.elapsed().as_millis());
            return Ok(());
        }
        AnisetteCommands::Reset => {
            settings.reset_identity(&path)?;
            return Ok(());
        }
        AnisetteCommands::Servers => {
            let servers = match remote_servers::fetch_server_list().await {
                Ok(servers) if !servers.is_empty() => servers,
                result => {
                    if let Err(error) = result {
                        log::warn!("Showing built-in servers: {error}");
                    }
                    remote_servers::builtin_servers()
                }
            };
            for server in servers {
                println!("{}\t{}", server.name, server.address);
            }
            return Ok(());
        }
        AnisetteCommands::Use { url } => {
            let server = AnisetteServer::new("Custom", url);
            use_server(&mut settings, server, RemoteProvider::Custom);
        }
        AnisetteCommands::Fastest => {
            let (server, elapsed) = remote_servers::rank_servers()
                .await
                .into_iter()
                .next()
                .ok_or_else(|| anyhow::anyhow!("No server responded"))?;
            println!("{} ms", elapsed.as_millis());
            use_server(&mut settings, server, RemoteProvider::SideStore);
        }
        AnisetteCommands::SideloadlyLogin { patreon } => {
            let (account, server) = if patreon {
                let auth = SideloadlyAuth::default();
                println!("Open this link, then paste the completed link here:");
                println!("{}", auth.patreon_login_url());
                let mut completion = String::new();
                std::io::stdin().read_line(&mut completion)?;
                let server = auth
                    .complete_patreon_login(&path, completion.trim())
                    .await?;
                (SideloadlyAccount::Patreon, server)
            } else {
                let server = SideloadlyAuth::connect_anonymous(&path).await?;
                (SideloadlyAccount::Anonymous, server)
            };
            settings.sideloadly_session = Some(SideloadlySession {
                account,
                server: server.clone(),
            });
            use_server(&mut settings, server, RemoteProvider::Sideloadly);
        }
        AnisetteCommands::SideloadlyLogout => {
            settings.logout_sideloadly();
        }
        AnisetteCommands::SideloadlyStatus => {
            let session = settings
                .sideloadly_session
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Not signed in to Sideloadly"))?;
            SideloadlyAuth::check_connection(&session.server).await?;
            println!("Available");
            return Ok(());
        }
        AnisetteCommands::SideloadlyRefresh => {
            let session = settings
                .sideloadly_session
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("Not signed in to Sideloadly"))?;
            let previous = session.server.clone();
            let server = SideloadlyAuth::refresh_device_token(&path, &previous).await?;
            session.server = server.clone();
            if settings.remote_server == previous {
                settings.remote_server = server;
            }
        }
    }
    settings.save_sync(&path)?;
    Ok(())
}
