mod account;
mod developer;
mod jit;
mod logs;
mod pairing;
mod server_info;
mod source_manager;
mod tailscale;

pub(crate) use logs::Message as LogsMessage;
pub(crate) use pairing::Message as PairingMessage;
pub(crate) use tailscale::Change as TailscaleChange;

use iced::widget::{
    button, checkbox, column, container, pick_list, row, scrollable, text, text_input,
};
use iced::{Alignment, Element, Fill, Task};
use plume_core::remote_anisette_v1;
use plume_core::remote_servers::{self, AnisetteServer};
use plume_store::{
    AccountStore, AnisetteMode, AnisetteSettings, AppearanceMode, RemoteProvider,
    SideloadlyAccount, SideloadlyAuth, SideloadlySession,
};
use plume_utils::Device;
use rust_i18n::t;

use crate::appearance;
use crate::subscriptions::run_on_tokio;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Team {
    pub name: String,
    pub id: String,
}

impl std::fmt::Display for Team {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.name, self.id)
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    ShowLogin(Option<String>),
    SelectAccount(String),
    RemoveAccount(String),
    Noop,
    ExportP12,
    CertificateExported(Result<(), String>),
    SelectTeam(String, String),
    FetchTeams(String),
    TeamsLoaded(String, u64, Result<Vec<Team>, String>),
    ProbeRemoteServer,
    ProbePreviewServer,
    RemoteServerProbed(
        ProbeTarget,
        u64,
        String,
        Result<plume_core::remote_probe::ServerInfo, String>,
    ),
    ToggleAutoStart(bool),
    SelectLocale(Option<String>),
    ResetAnisetteData,
    SelectAnisetteMode(AnisetteModeChoice),
    SetupLocalAnisette,
    LocalAnisetteReady(AnisetteMode, Result<(), String>),
    SelectAnisetteServer(AnisetteServer),
    ApplyAnisetteServer,
    FetchAnisetteServers,
    AnisetteServersLoaded(Result<Vec<AnisetteServer>, String>),
    UpdateCustomAnisetteServer(String),
    UseCustomAnisetteServer,
    TestAnisette,
    AnisetteTested(Result<std::time::Duration, String>),
    FindFastestAnisetteServer,
    FastestAnisetteServerFound(Option<AnisetteServer>),
    SelectRemoteProvider(RemoteProviderChoice),
    SideloadlyLogin,
    SideloadlyLoggedIn(SideloadlyAccount, Result<AnisetteServer, String>),
    SideloadlyRefreshDeviceToken,
    SideloadlyDeviceTokenRefreshed(Result<AnisetteServer, String>),
    SideloadlyPatreonLogin,
    UpdateSideloadlyCompletion(PatreonLoginInput),
    CompleteSideloadlyPatreonLogin,
    CancelSideloadlyPatreonLogin,
    SideloadlyLogout,
    SideloadlyAccountStatus,
    SideloadlyAccountChecked(Result<(), String>),
    ToggleKeyring(bool),
    SelectAppearance(AppearanceChoice),
    SelectTab(SettingsTab),
    Pairing(pairing::Message),
    Developer(developer::Message),
    Sources(source_manager::Message),
    Logs(logs::Message),
    ToggleLockDevice(bool),
    ToggleCloseToTray(bool),
    Tailscale(tailscale::Message),
    TailscaleChanged(tailscale::Change),
    Jit(jit::Message),
    JitChanged(plume_store::JitServerSettings),
}

#[derive(Clone, Default)]
pub struct PatreonLoginInput(String);

impl std::fmt::Debug for PatreonLoginInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[redacted Patreon login input]")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsTab {
    General,
    Behavior,
    RemoteAnisette,
    Account,
    Pairing,
    Developer,
    Sources,
    Tailscale,
    Jit,
    Logs,
}

impl SettingsTab {
    const ALL: [SettingsTab; 10] = [
        SettingsTab::General,
        SettingsTab::Behavior,
        SettingsTab::RemoteAnisette,
        SettingsTab::Account,
        SettingsTab::Pairing,
        SettingsTab::Developer,
        SettingsTab::Sources,
        SettingsTab::Tailscale,
        SettingsTab::Jit,
        SettingsTab::Logs,
    ];

    fn label(self) -> std::borrow::Cow<'static, str> {
        match self {
            SettingsTab::General => t!("settings_tab_general"),
            SettingsTab::Behavior => t!("settings_tab_behavior"),
            SettingsTab::RemoteAnisette => t!("settings_tab_remote_anisette"),
            SettingsTab::Account => t!("settings_tab_account"),
            SettingsTab::Pairing => t!("settings_tab_pairing"),
            SettingsTab::Developer => t!("settings_tab_developer"),
            SettingsTab::Sources => t!("settings_tab_sources"),
            SettingsTab::Tailscale => t!("settings_tab_tailscale"),
            SettingsTab::Jit => t!("settings_tab_jit"),
            SettingsTab::Logs => t!("settings_tab_logs"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnisetteModeChoice(pub AnisetteMode);

impl std::fmt::Display for AnisetteModeChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self.0 {
            AnisetteMode::Local => t!("settings_anisette_local"),
            AnisetteMode::Remote => t!("settings_anisette_remote"),
            AnisetteMode::Fake => t!("settings_anisette_fake"),
        };
        write!(f, "{}", name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteProviderChoice(pub RemoteProvider);

#[derive(Debug, Clone, Copy)]
pub enum ProbeTarget {
    Current,
    Preview,
}

impl std::fmt::Display for RemoteProviderChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self.0 {
            RemoteProvider::SideStore => t!("settings_anisette_sidestore"),
            RemoteProvider::Sideloadly => t!("settings_anisette_sideloadly"),
            RemoteProvider::Custom => t!("settings_anisette_custom"),
        };
        write!(f, "{}", name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppearanceChoice(pub AppearanceMode);

impl std::fmt::Display for AppearanceChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self.0 {
            AppearanceMode::System => t!("settings_appearance_system"),
            AppearanceMode::Light => t!("settings_appearance_light"),
            AppearanceMode::Dark => t!("settings_appearance_dark"),
        };
        write!(f, "{}", name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocaleChoice {
    code: Option<String>,
}

impl LocaleChoice {
    fn system() -> Self {
        Self { code: None }
    }

    fn explicit(code: String) -> Self {
        Self { code: Some(code) }
    }

    fn code(&self) -> Option<&str> {
        self.code.as_deref()
    }
}

impl std::fmt::Display for LocaleChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.code {
            None => write!(f, "{}", rust_i18n::t!("settings_system_language")),
            Some(code) => {
                let name = rust_i18n::t!("_language_name", locale = code);
                write!(f, "{}", name)
            }
        }
    }
}

#[derive(Debug)]
pub struct SettingsScreen {
    account: account::AccountTab,
    server_info: server_info::ServerInfoPanel,
    preview_info: server_info::ServerInfoPanel,
    remote_provider: RemoteProvider,
    pending_server: Option<AnisetteServer>,
    anisette: AnisetteSettings,
    anisette_servers: Vec<AnisetteServer>,
    loading_anisette_servers: bool,
    custom_anisette_server: String,
    anisette_status: Option<String>,
    testing_anisette: bool,
    sideloadly_logging_in: bool,
    sideloadly_patreon_pending: bool,
    sideloadly_completion: PatreonLoginInput,
    pub tab: SettingsTab,
    pairing: pairing::PairingTab,
    developer: developer::DeveloperTab,
    sources: source_manager::SourcesTab,
    tailscale: tailscale::TailscaleTab,
    jit: jit::JitTab,
    logs: logs::LogsTab,
}

impl SettingsScreen {
    pub fn new() -> Self {
        let anisette = AnisetteSettings::load_sync(&crate::defaults::get_data_path());
        let remote_provider = if anisette.remote_server.name == t!("settings_anisette_custom") {
            RemoteProvider::Custom
        } else {
            anisette.remote_provider
        };
        let custom_anisette_server = if remote_provider == RemoteProvider::Custom {
            anisette.remote_server.address.clone()
        } else {
            String::new()
        };
        let mut screen = Self {
            account: account::AccountTab::default(),
            server_info: server_info::ServerInfoPanel::default(),
            preview_info: server_info::ServerInfoPanel::default(),
            remote_provider,
            pending_server: Some(anisette.remote_server.clone()),
            anisette,
            anisette_servers: remote_servers::builtin_servers(),
            loading_anisette_servers: false,
            custom_anisette_server,
            anisette_status: None,
            testing_anisette: false,
            sideloadly_logging_in: false,
            sideloadly_patreon_pending: false,
            sideloadly_completion: PatreonLoginInput::default(),
            tab: SettingsTab::General,
            pairing: pairing::PairingTab::new(),
            developer: developer::DeveloperTab::default(),
            sources: source_manager::SourcesTab::new(),
            tailscale: tailscale::TailscaleTab::new(Default::default()),
            jit: jit::JitTab::new(Default::default()),
            logs: logs::LogsTab::default(),
        };
        screen.ensure_selected_server_listed();
        screen
    }

    fn ensure_selected_server_listed(&mut self) {
        if self.anisette.remote_provider != RemoteProvider::SideStore {
            return;
        }
        let selected = &self.anisette.remote_server;
        if !self
            .anisette_servers
            .iter()
            .any(|s| s.address == selected.address)
        {
            self.anisette_servers.insert(0, selected.clone());
        }
    }

    fn can_select_server(&self) -> bool {
        !self.testing_anisette
            && !self.sideloadly_logging_in
            && self
                .pending_server
                .as_ref()
                .is_some_and(|server| server.address != self.anisette.remote_server.address)
    }

    fn save_anisette(&mut self) -> bool {
        if let Err(err) = self.anisette.save_sync(&crate::defaults::get_data_path()) {
            log::error!("Failed to save anisette settings: {err}");
            self.anisette_status = Some(err.to_string());
            return false;
        }
        true
    }

    fn select_anisette_server(&mut self, server: AnisetteServer) -> bool {
        log::info!("Selected anisette server {server}");
        self.server_info.clear();
        self.anisette.remote_server = server;
        self.anisette_status = None;
        self.ensure_selected_server_listed();
        self.save_anisette()
    }

    pub fn account_changed(&mut self, email: &str) {
        self.account.forget(email);
    }

    pub fn update(&mut self, message: Message, store: Option<&AccountStore>) -> Task<Message> {
        match message {
            Message::CertificateExported(result) => {
                if let Err(error) = result {
                    log::error!("Certificate export failed: {error}");
                    return Task::perform(
                        async move {
                            rfd::AsyncMessageDialog::new()
                                .set_title("Certificate export failed")
                                .set_description(&error)
                                .set_level(rfd::MessageLevel::Error)
                                .show()
                                .await;
                        },
                        |_| Message::Noop,
                    );
                }
                Task::none()
            }
            Message::SelectTab(tab) => {
                self.tab = tab;
                match tab {
                    SettingsTab::General => self.update(Message::ProbeRemoteServer, store),
                    SettingsTab::RemoteAnisette => self.update(Message::ProbePreviewServer, store),
                    SettingsTab::Account => {
                        if let Some(account) = store.and_then(|store| store.selected_account()) {
                            self.account.check(account.email().clone(), store)
                        } else {
                            Task::none()
                        }
                    }
                    SettingsTab::Pairing => {
                        self.update(Message::Pairing(pairing::Message::Refresh), store)
                    }
                    SettingsTab::Developer => {
                        self.update(Message::Developer(developer::Message::Load), store)
                    }
                    SettingsTab::Tailscale => {
                        if let Some(store) = store {
                            self.tailscale = tailscale::TailscaleTab::new(store.tailscale().clone());
                        }
                        Task::none()
                    }
                    SettingsTab::Jit => {
                        if let Some(store) = store {
                            self.jit = jit::JitTab::new(store.jit_server().clone());
                        }
                        Task::none()
                    }
                    _ => Task::none(),
                }
            }
            Message::Pairing(msg) => {
                let remote = store.is_some_and(|s| s.remote_pairing());
                self.pairing.update(msg, remote).map(Message::Pairing)
            }
            Message::Developer(msg) => {
                let account = store.and_then(|s| s.selected_account());
                self.developer.update(msg, account).map(Message::Developer)
            }
            Message::Sources(msg) => self.sources.update(msg).map(Message::Sources),
            Message::Logs(msg) => self.logs.update(msg).map(Message::Logs),
            Message::Tailscale(msg) => {
                let (task, change) = self.tailscale.update(msg);
                let task = task.map(Message::Tailscale);
                match change {
                    Some(change) => Task::batch([task, Task::done(Message::TailscaleChanged(change))]),
                    None => task,
                }
            }
            Message::Jit(msg) => {
                let (task, settings) = self.jit.update(msg);
                let task = task.map(Message::Jit);
                match settings {
                    Some(settings) => Task::batch([task, Task::done(Message::JitChanged(settings))]),
                    None => task,
                }
            }
            Message::FetchTeams(email) => self.account.check(email, store),
            Message::TeamsLoaded(email, request, result) => {
                self.account.checked(email, request, result);
                Task::none()
            }
            Message::ProbeRemoteServer => match self.anisette.mode {
                AnisetteMode::Remote => self
                    .server_info
                    .probe(&self.anisette.remote_server, ProbeTarget::Current),
                AnisetteMode::Local => self.server_info.probe_local(
                    self.anisette
                        .configuration(&crate::defaults::get_data_path()),
                ),
                AnisetteMode::Fake => Task::none(),
            },
            Message::ProbePreviewServer => match &self.pending_server {
                Some(server) => self.preview_info.probe(server, ProbeTarget::Preview),
                None => Task::none(),
            },
            Message::RemoteServerProbed(target, request, address, result) => {
                match target {
                    ProbeTarget::Current => self.server_info.probed(request, address, result),
                    ProbeTarget::Preview => self.preview_info.probed(request, address, result),
                }
                Task::none()
            }
            Message::ToggleAutoStart(_) => Task::none(),
            Message::SelectTeam(_, _) => Task::none(),
            Message::SelectLocale(_) => Task::none(),
            Message::ResetAnisetteData => {
                if self.testing_anisette {
                    return Task::none();
                }
                self.anisette_status = Some(
                    match self
                        .anisette
                        .reset_identity(&crate::defaults::get_data_path())
                    {
                        Ok(()) => t!("settings_anisette_reset_done").to_string(),
                        Err(err) => err.to_string(),
                    },
                );
                Task::none()
            }
            Message::SelectAnisetteMode(AnisetteModeChoice(mode)) => {
                if self.testing_anisette || self.sideloadly_logging_in {
                    return Task::none();
                }
                self.server_info.clear();
                self.anisette.mode = mode;
                self.anisette_status = None;
                if self.save_anisette() {
                    return match mode {
                        AnisetteMode::Local => self.update(Message::ProbeRemoteServer, store),
                        AnisetteMode::Remote => self.update(Message::ProbeRemoteServer, store),
                        AnisetteMode::Fake => Task::none(),
                    };
                }
                Task::none()
            }
            Message::SetupLocalAnisette => {
                let mode = self.anisette.mode;
                if self.testing_anisette || mode == AnisetteMode::Remote {
                    return Task::none();
                }
                self.testing_anisette = true;
                self.anisette_status = Some(t!("settings_anisette_local_setting_up").to_string());
                let config = self
                    .anisette
                    .configuration(&crate::defaults::get_data_path());
                Task::perform(
                    run_on_tokio(async move {
                        plume_core::auth::anisette_data::AnisetteData::new(config)
                            .await
                            .map(|_| ())
                            .map_err(|e| e.to_string())
                    }),
                    move |result| Message::LocalAnisetteReady(mode, result),
                )
            }
            Message::LocalAnisetteReady(mode, result) => {
                self.testing_anisette = false;
                if self.anisette.mode == mode {
                    self.anisette_status = Some(match result {
                        Ok(()) => t!("settings_anisette_local_ready").to_string(),
                        Err(err) => {
                            t!("settings_anisette_local_setup_failed", error = err).to_string()
                        }
                    });
                }
                Task::none()
            }
            Message::SelectAnisetteServer(server) => {
                if self.testing_anisette || self.sideloadly_logging_in {
                    return Task::none();
                }
                self.pending_server = Some(server);
                self.anisette_status = None;
                self.update(Message::ProbePreviewServer, store)
            }
            Message::ApplyAnisetteServer => {
                if !self.can_select_server() {
                    return Task::none();
                }
                let Some(server) = self.pending_server.clone() else {
                    return Task::none();
                };
                if server.address == self.anisette.remote_server.address {
                    return Task::none();
                }
                let previous = self.anisette.clone();
                self.anisette.remote_provider = self.remote_provider;
                if self.select_anisette_server(server) {
                    self.update(Message::ProbeRemoteServer, store)
                } else {
                    self.anisette = previous;
                    Task::none()
                }
            }
            Message::FetchAnisetteServers => {
                if self.loading_anisette_servers {
                    return Task::none();
                }
                self.loading_anisette_servers = true;
                Task::perform(
                    run_on_tokio(async {
                        remote_servers::fetch_server_list()
                            .await
                            .map_err(|e| e.to_string())
                    }),
                    Message::AnisetteServersLoaded,
                )
            }
            Message::AnisetteServersLoaded(result) => {
                self.loading_anisette_servers = false;
                match result {
                    Ok(servers) if !servers.is_empty() => {
                        self.anisette_servers = servers;
                        self.ensure_selected_server_listed();
                        if self.remote_provider == RemoteProvider::SideStore
                            && let Some(server) = &self.pending_server
                            && !self
                                .anisette_servers
                                .iter()
                                .any(|s| s.address == server.address)
                        {
                            self.anisette_servers.insert(0, server.clone());
                        }
                    }
                    Ok(_) => {}
                    Err(err) => {
                        log::error!("Failed to fetch anisette servers: {err}");
                        self.anisette_status =
                            Some(t!("settings_anisette_servers_failed", error = err).to_string());
                    }
                }
                Task::none()
            }
            Message::UpdateCustomAnisetteServer(url) => {
                self.custom_anisette_server = url;
                self.pending_server = None;
                self.preview_info.clear();
                self.anisette_status = None;
                Task::none()
            }
            Message::UseCustomAnisetteServer => {
                if self.testing_anisette || self.sideloadly_logging_in {
                    return Task::none();
                }
                let input = self.custom_anisette_server.trim();
                let server = if reqwest::Url::parse(input).is_ok_and(|url| {
                    matches!(url.scheme(), "http" | "https") && url.host_str().is_some()
                }) {
                    AnisetteServer::new(t!("settings_anisette_custom"), input)
                } else if let Some(server) = parse_sideloadly_shortcut(input) {
                    // A Sideloadly provider shortcut, optionally with a self-hosted host.
                    server
                } else {
                    self.anisette_status = Some(t!("settings_anisette_invalid_url").to_string());
                    return Task::none();
                };
                self.pending_server = Some(server);
                self.anisette_status = None;
                self.update(Message::ProbePreviewServer, store)
            }
            Message::TestAnisette => {
                if self.testing_anisette {
                    return Task::none();
                }
                self.testing_anisette = true;
                self.anisette_status = Some(t!("settings_anisette_testing").to_string());
                let config = self
                    .anisette
                    .configuration(&crate::defaults::get_data_path());
                Task::perform(
                    run_on_tokio(async move {
                        let start = std::time::Instant::now();
                        plume_core::auth::anisette_data::AnisetteData::new(config)
                            .await
                            .map(|_| start.elapsed())
                            .map_err(|e| e.to_string())
                    }),
                    Message::AnisetteTested,
                )
            }
            Message::AnisetteTested(result) => {
                self.testing_anisette = false;
                self.anisette_status = Some(match result {
                    Ok(elapsed) => t!(
                        "settings_anisette_test_ok",
                        ms = elapsed.as_millis().to_string()
                    )
                    .to_string(),
                    Err(err) => t!("settings_anisette_test_failed", error = err).to_string(),
                });
                Task::none()
            }
            Message::FindFastestAnisetteServer => {
                if self.testing_anisette {
                    return Task::none();
                }
                self.testing_anisette = true;
                self.anisette_status = Some(t!("settings_anisette_finding_fastest").to_string());
                Task::perform(
                    run_on_tokio(async {
                        remote_servers::rank_servers()
                            .await
                            .into_iter()
                            .next()
                            .map(|(server, _)| server)
                    }),
                    Message::FastestAnisetteServerFound,
                )
            }
            Message::FastestAnisetteServerFound(server) => {
                self.testing_anisette = false;
                match server {
                    Some(server) => {
                        let status = t!(
                            "settings_anisette_fastest_found",
                            name = server.name.clone()
                        )
                        .to_string();
                        self.pending_server = Some(server);
                        self.anisette_status = Some(status);
                        return self.update(Message::ProbePreviewServer, store);
                    }
                    None => {
                        self.anisette_status =
                            Some(t!("settings_anisette_none_responded").to_string());
                    }
                }
                Task::none()
            }
            Message::SelectRemoteProvider(RemoteProviderChoice(provider)) => {
                if self.testing_anisette || self.sideloadly_logging_in {
                    return Task::none();
                }
                self.preview_info.clear();
                self.remote_provider = provider;
                self.anisette_status = None;
                self.pending_server = match provider {
                    RemoteProvider::Sideloadly => self
                        .anisette
                        .sideloadly_session
                        .as_ref()
                        .map(|s| s.server.clone()),
                    RemoteProvider::SideStore => {
                        Some(if self.anisette.remote_provider == provider {
                            self.anisette.remote_server.clone()
                        } else {
                            remote_servers::builtin_servers().remove(0)
                        })
                    }
                    RemoteProvider::Custom => {
                        if self.anisette.remote_provider == provider
                            && self.custom_anisette_server.is_empty()
                        {
                            self.custom_anisette_server =
                                self.anisette.remote_server.address.clone();
                        }
                        None
                    }
                };
                self.update(Message::ProbePreviewServer, store)
            }
            Message::SideloadlyLogin => {
                if self.sideloadly_logging_in {
                    return Task::none();
                }
                self.sideloadly_logging_in = true;
                self.sideloadly_patreon_pending = false;
                self.sideloadly_completion = PatreonLoginInput::default();
                self.anisette_status =
                    Some(t!("settings_anisette_sideloadly_logging_in").to_string());
                let data_path = crate::defaults::get_data_path();
                Task::perform(
                    run_on_tokio(async move {
                        plume_store::SideloadlyAuth::connect_anonymous(&data_path)
                            .await
                            .map_err(|e| e.to_string())
                    }),
                    |result| Message::SideloadlyLoggedIn(SideloadlyAccount::Anonymous, result),
                )
            }
            Message::SideloadlyLoggedIn(account, result) => {
                self.sideloadly_logging_in = false;
                match result {
                    Ok(server) => {
                        self.sideloadly_patreon_pending = false;
                        self.sideloadly_completion = PatreonLoginInput::default();
                        self.anisette.sideloadly_session = Some(SideloadlySession {
                            account,
                            server: server.clone(),
                        });
                        if self.remote_provider == RemoteProvider::Sideloadly {
                            self.pending_server = Some(server.clone());
                        }
                        if self.save_anisette() {
                            self.anisette_status = Some(
                                t!(
                                    "settings_anisette_sideloadly_login_ok",
                                    name = server.name.clone()
                                )
                                .to_string(),
                            );
                        }
                    }
                    Err(err) => {
                        self.anisette_status = Some(
                            t!("settings_anisette_sideloadly_login_failed", error = err)
                                .to_string(),
                        );
                    }
                }
                self.update(Message::ProbePreviewServer, store)
            }
            Message::SideloadlyRefreshDeviceToken => {
                if self.sideloadly_logging_in {
                    return Task::none();
                }
                let Some(session) = self.anisette.sideloadly_session.clone() else {
                    return Task::none();
                };
                self.sideloadly_logging_in = true;
                self.anisette_status = Some(t!("settings_sideloadly_refreshing").to_string());
                let path = crate::defaults::get_data_path();
                Task::perform(
                    run_on_tokio(async move {
                        SideloadlyAuth::refresh_device_token(&path, &session.server)
                            .await
                            .map_err(|e| e.to_string())
                    }),
                    Message::SideloadlyDeviceTokenRefreshed,
                )
            }
            Message::SideloadlyDeviceTokenRefreshed(result) => {
                self.sideloadly_logging_in = false;
                match result {
                    Ok(server) => {
                        if let Some(session) = &mut self.anisette.sideloadly_session {
                            let previous = std::mem::replace(&mut session.server, server.clone());
                            self.anisette_servers.retain(|s| *s != previous);
                            if self.remote_provider == RemoteProvider::Sideloadly {
                                self.pending_server = Some(server.clone());
                            }
                            let saved =
                                if self.anisette.remote_provider == RemoteProvider::Sideloadly {
                                    self.select_anisette_server(server)
                                } else {
                                    self.save_anisette()
                                };
                            if saved {
                                self.anisette_status =
                                    Some(t!("settings_sideloadly_refreshed").to_string());
                            }
                        }
                    }
                    Err(err) => self.anisette_status = Some(err),
                }
                Task::batch([
                    self.update(Message::ProbeRemoteServer, store),
                    self.update(Message::ProbePreviewServer, store),
                ])
            }
            Message::SideloadlyPatreonLogin => {
                if self.sideloadly_logging_in {
                    return Task::none();
                }
                match open::that(SideloadlyAuth::default().patreon_login_url()) {
                    Ok(()) => {
                        self.sideloadly_patreon_pending = true;
                        self.sideloadly_completion = PatreonLoginInput::default();
                        self.anisette_status =
                            Some(t!("settings_sideloadly_patreon_instructions").to_string());
                    }
                    Err(err) => self.anisette_status = Some(err.to_string()),
                }
                Task::none()
            }
            Message::UpdateSideloadlyCompletion(value) => {
                self.sideloadly_completion = value;
                Task::none()
            }
            Message::CompleteSideloadlyPatreonLogin => {
                if self.sideloadly_logging_in || self.sideloadly_completion.0.trim().is_empty() {
                    return Task::none();
                }
                self.sideloadly_logging_in = true;
                let completion = std::mem::take(&mut self.sideloadly_completion).0;
                let path = crate::defaults::get_data_path();
                Task::perform(
                    run_on_tokio(async move {
                        SideloadlyAuth::default()
                            .complete_patreon_login(&path, &completion)
                            .await
                            .map_err(|e| e.to_string())
                    }),
                    |result| Message::SideloadlyLoggedIn(SideloadlyAccount::Patreon, result),
                )
            }
            Message::CancelSideloadlyPatreonLogin => {
                self.sideloadly_patreon_pending = false;
                self.sideloadly_completion = PatreonLoginInput::default();
                Task::none()
            }
            Message::SideloadlyLogout => {
                if self.sideloadly_logging_in {
                    return Task::none();
                }
                if let Some(session) = self.anisette.logout_sideloadly() {
                    self.anisette_servers.retain(|s| *s != session.server);
                }
                if self.remote_provider == RemoteProvider::Sideloadly {
                    self.pending_server = None;
                    self.preview_info.clear();
                }
                self.server_info.clear();
                self.sideloadly_patreon_pending = false;
                self.sideloadly_completion = PatreonLoginInput::default();
                self.ensure_selected_server_listed();
                if self.save_anisette() {
                    self.anisette_status = Some(t!("settings_sideloadly_logged_out").to_string());
                }
                self.update(Message::ProbeRemoteServer, store)
            }
            Message::SideloadlyAccountStatus => {
                if self.sideloadly_logging_in {
                    return Task::none();
                }
                let Some(session) = self.anisette.sideloadly_session.clone() else {
                    self.anisette_status = Some(t!("settings_sideloadly_signed_out").to_string());
                    return Task::none();
                };
                self.sideloadly_logging_in = true;
                self.anisette_status = Some(t!("settings_sideloadly_checking").to_string());
                Task::perform(
                    run_on_tokio(async move {
                        SideloadlyAuth::check_connection(&session.server)
                            .await
                            .map_err(|e| e.to_string())
                    }),
                    Message::SideloadlyAccountChecked,
                )
            }
            Message::SideloadlyAccountChecked(result) => {
                self.sideloadly_logging_in = false;
                self.anisette_status = Some(match result {
                    Ok(()) => t!("settings_sideloadly_available").to_string(),
                    Err(err) => t!("settings_sideloadly_unavailable", error = err).to_string(),
                });
                Task::none()
            }
            _ => Task::none(),
        }
    }

    pub fn view<'a>(
        &'a self,
        account_store: &'a Option<AccountStore>,
        selected_locale: &'a Option<String>,
        selected_device: Option<&'a Device>,
        devices: &'a [Device],
    ) -> Element<'a, Message> {
        let remote_pairing = account_store.as_ref().is_some_and(|s| s.remote_pairing());
        let account = account_store.as_ref().and_then(|s| s.selected_account());
        let body = match self.tab {
            SettingsTab::General => self.view_general(account_store, selected_locale),
            SettingsTab::Behavior => self.view_behavior(account_store),
            SettingsTab::RemoteAnisette => self.view_remote_anisette(),
            SettingsTab::Account => self.account.view(account_store.as_ref()),
            SettingsTab::Pairing => self
                .pairing
                .view(remote_pairing, selected_device, devices)
                .map(Message::Pairing),
            SettingsTab::Developer => self.developer.view(account).map(Message::Developer),
            SettingsTab::Sources => self.sources.view().map(Message::Sources),
            SettingsTab::Tailscale => self.tailscale.view().map(Message::Tailscale),
            SettingsTab::Jit => self.jit.view().map(Message::Jit),
            SettingsTab::Logs => self.logs.view().map(Message::Logs),
        };

        let account_hero = if let Some(account) = account {
            let name = if account.first_name().is_empty() {
                account.email()
            } else {
                account.first_name()
            };
            let mut details = column![
                text(name).size(appearance::THEME_FONT_SIZE + 2.0),
                text(account.email()).size(appearance::THEME_FONT_SIZE - 1.0),
            ]
            .spacing(appearance::THEME_PADDING / 2.0);
            if !account.team_id().is_empty() {
                details = details.push(
                    text(format!("{} {}", t!("account_team"), account.team_id()))
                        .size(appearance::THEME_FONT_SIZE - 1.0),
                );
            }
            container(details)
                .padding(appearance::THEME_PADDING)
                .style(container::rounded_box)
                .width(Fill)
        } else {
            container(text(t!("account_signed_out")))
                .padding(appearance::THEME_PADDING)
                .style(container::rounded_box)
                .width(Fill)
        };
        let mut sidebar = column![account_hero]
            .spacing(appearance::THEME_PADDING)
            .width(210);
        for tab in SettingsTab::ALL {
            let style = if tab == self.tab {
                appearance::p_button
            } else {
                appearance::s_button
            };
            sidebar = sidebar.push(
                button(text(tab.label()).align_x(Alignment::Start))
                    .on_press(Message::SelectTab(tab))
                    .style(style)
                    .width(Fill),
            );
        }

        row![sidebar, container(body).width(Fill).height(Fill)]
            .height(Fill)
            .spacing(appearance::THEME_PADDING)
            .into()
    }

    fn view_general<'a>(
        &'a self,
        account_store: &'a Option<AccountStore>,
        selected_locale: &'a Option<String>,
    ) -> Element<'a, Message> {
        let Some(store) = account_store else {
            return column![text(t!("settings_loading_accounts"))]
                .spacing(appearance::THEME_PADDING)
                .padding(appearance::THEME_PADDING)
                .into();
        };

        let mut content = column![].spacing(appearance::THEME_PADDING);

        let auto_start_enabled = crate::startup::auto_start_enabled();
        content = content.push(self.view_auto_start_toggle(auto_start_enabled));
        content = content.push(self.view_keyring_toggle(store));
        content = content.push(self.view_appearance_and_language(store, selected_locale));
        content = content.push(self.view_anisette());
        scrollable(
            content
                .padding([0.0, appearance::THEME_PADDING])
                .width(Fill),
        )
        .height(Fill)
        .into()
    }

    fn view_behavior<'a>(&'a self, account_store: &'a Option<AccountStore>) -> Element<'a, Message> {
        let small = appearance::THEME_FONT_SIZE - 1.0;
        let (lock_device, close_to_tray) = account_store
            .as_ref()
            .map_or((false, true), |store| (store.lock_device(), store.close_to_tray()));
        scrollable(
            column![
                checkbox(lock_device)
                    .label(t!("settings_lock_device"))
                    .on_toggle(Message::ToggleLockDevice),
                text(t!("settings_lock_device_desc")).size(small),
                checkbox(close_to_tray)
                    .label(t!("settings_close_to_tray"))
                    .on_toggle(Message::ToggleCloseToTray),
                text(t!("settings_close_to_tray_desc")).size(small),
            ]
            .spacing(appearance::THEME_PADDING)
            .padding([0.0, appearance::THEME_PADDING])
            .width(Fill),
        )
        .height(Fill)
        .into()
    }

    fn view_anisette(&self) -> Element<'_, Message> {
        let mode = self.anisette.mode;

        let mode_picker = pick_list(
            AnisetteMode::ALL.map(AnisetteModeChoice),
            Some(AnisetteModeChoice(mode)),
            Message::SelectAnisetteMode,
        )
        .style(appearance::s_pick_list);

        let mut section =
            column![text(t!("settings_anisette")), mode_picker,].spacing(appearance::THEME_PADDING);

        match mode {
            AnisetteMode::Remote => {
                section = section.push(
                    self.server_info
                        .view(&self.anisette.remote_server, Message::ProbeRemoteServer),
                );
            }
            AnisetteMode::Local => {
                section = section.push(
                    text(t!("settings_anisette_local_desc"))
                        .size(appearance::THEME_FONT_SIZE - 1.0),
                );
                section = section.push(self.server_info.view(
                    &AnisetteServer::new(t!("settings_anisette_local"), "local"),
                    Message::ProbeRemoteServer,
                ));
            }
            AnisetteMode::Fake => {
                section = section.push(
                    text(t!("settings_anisette_fake_desc")).size(appearance::THEME_FONT_SIZE - 1.0),
                );
                section = section.push(
                    button(text(t!("settings_anisette_fake_setup")))
                        .on_press_maybe(
                            (!self.testing_anisette).then_some(Message::SetupLocalAnisette),
                        )
                        .style(appearance::s_button),
                );
            }
        }

        let test_button = button(text(t!("settings_anisette_test")))
            .on_press_maybe((!self.testing_anisette).then_some(Message::TestAnisette))
            .style(appearance::s_button);
        let reset_button = button(text(t!("settings_reset_adi")))
            .on_press_maybe((!self.testing_anisette).then_some(Message::ResetAnisetteData))
            .style(appearance::s_button);

        section = section.push(
            row![test_button, reset_button]
                .spacing(appearance::THEME_PADDING)
                .align_y(Alignment::Center),
        );
        if let Some(status) = &self.anisette_status {
            section = section.push(text(status).size(appearance::THEME_FONT_SIZE - 1.0));
        }
        container(section)
            .padding(appearance::THEME_PADDING)
            .style(container::rounded_box)
            .width(Fill)
            .into()
    }

    fn view_remote_anisette(&self) -> Element<'_, Message> {
        let idle = !self.testing_anisette && !self.sideloadly_logging_in;
        let mut content = column![
            text(t!("settings_tab_remote_anisette")),
            pick_list(
                RemoteProvider::ALL.map(RemoteProviderChoice),
                Some(RemoteProviderChoice(self.remote_provider)),
                Message::SelectRemoteProvider,
            )
            .style(appearance::s_pick_list),
        ]
        .spacing(appearance::THEME_PADDING);

        match self.remote_provider {
            RemoteProvider::SideStore => {
                content = content.push(
                    pick_list(
                        self.anisette_servers.clone(),
                        self.pending_server.clone(),
                        Message::SelectAnisetteServer,
                    )
                    .placeholder(t!("settings_anisette_loading_servers"))
                    .on_open(Message::FetchAnisetteServers)
                    .style(appearance::s_pick_list)
                    .width(Fill),
                );
            }
            RemoteProvider::Sideloadly => {}
            RemoteProvider::Custom => {
                content = content.push(
                    row![
                        text_input(
                            &t!("settings_anisette_custom_placeholder"),
                            &self.custom_anisette_server,
                        )
                        .on_input(Message::UpdateCustomAnisetteServer)
                        .on_submit(Message::UseCustomAnisetteServer)
                        .padding(6)
                        .width(Fill),
                        button(text(t!("settings_anisette_test_server")))
                            .on_press_maybe(
                                (idle && !self.custom_anisette_server.trim().is_empty())
                                    .then_some(Message::UseCustomAnisetteServer)
                            )
                            .style(appearance::s_button),
                    ]
                    .spacing(appearance::THEME_PADDING)
                    .align_y(Alignment::Center),
                );
            }
        }
        if let Some(server) = &self.pending_server {
            content = content.push(self.preview_info.view(server, Message::ProbePreviewServer));
        }
        if self.remote_provider == RemoteProvider::Sideloadly {
            content = content.push(self.view_sideloadly_options());
        }
        if self.remote_provider == RemoteProvider::SideStore {
            content = content.push(
                button(text(t!("settings_anisette_find_fastest")))
                    .on_press_maybe(idle.then_some(Message::FindFastestAnisetteServer))
                    .style(appearance::s_button),
            );
        }
        if let Some(status) = &self.anisette_status {
            content = content.push(text(status).size(appearance::THEME_FONT_SIZE - 1.0));
        }
        let can_select = self.can_select_server();
        column![
            scrollable(
                content
                    .padding([0.0, appearance::THEME_PADDING])
                    .width(Fill)
            )
            .height(Fill),
            container(
                button(text(t!("settings_anisette_select_server")))
                    .on_press_maybe(can_select.then_some(Message::ApplyAnisetteServer))
                    .style(if can_select {
                        appearance::p_button
                    } else {
                        appearance::s_button
                    }),
            )
            .align_x(Alignment::End)
            .width(Fill)
            .padding(appearance::THEME_PADDING),
        ]
        .height(Fill)
        .spacing(appearance::THEME_PADDING)
        .into()
    }

    fn view_sideloadly_options(&self) -> Element<'_, Message> {
        let idle = !self.sideloadly_logging_in;
        let signed_in = self.anisette.sideloadly_session.is_some();
        let account = match self.anisette.sideloadly_session.as_ref().map(|s| s.account) {
            Some(SideloadlyAccount::Anonymous) => t!("settings_sideloadly_anonymous_status"),
            Some(SideloadlyAccount::Patreon) => t!("settings_sideloadly_patreon_status"),
            None => t!("settings_sideloadly_signed_out"),
        };
        let mut options = column![
            text(t!("settings_anisette_sideloadly_desc")).size(appearance::THEME_FONT_SIZE - 1.0),
            text(account),
        ]
        .spacing(appearance::THEME_PADDING);
        if !signed_in && !self.sideloadly_patreon_pending {
            options = options.push(
                button(text(t!("settings_sideloadly_login_anonymously")))
                    .on_press_maybe(idle.then_some(Message::SideloadlyLogin))
                    .style(appearance::p_button),
            );
        }
        if signed_in {
            options = options.push(
                row![
                    button(text(t!("settings_sideloadly_refresh_device_token")))
                        .on_press_maybe(idle.then_some(Message::SideloadlyRefreshDeviceToken))
                        .style(appearance::s_button),
                    button(text(t!("settings_sideloadly_account_status")))
                        .on_press_maybe(idle.then_some(Message::SideloadlyAccountStatus))
                        .style(appearance::s_button),
                ]
                .spacing(appearance::THEME_PADDING),
            );
        }
        let mut actions = row![].spacing(appearance::THEME_PADDING);
        if !self.sideloadly_patreon_pending
            && !self
                .anisette
                .sideloadly_session
                .as_ref()
                .is_some_and(|session| session.account == SideloadlyAccount::Patreon)
        {
            actions = actions.push(
                button(text(t!("settings_sideloadly_login_patreon")))
                    .on_press_maybe(idle.then_some(Message::SideloadlyPatreonLogin))
                    .style(appearance::s_button),
            );
        }
        if signed_in {
            actions = actions.push(
                button(text(t!("settings_sideloadly_logout")))
                    .on_press_maybe(idle.then_some(Message::SideloadlyLogout))
                    .style(appearance::s_button),
            );
        }
        options = options.push(actions);
        if self.sideloadly_patreon_pending {
            options = options.push(text(t!("settings_sideloadly_patreon_instructions")));
            options = options.push(
                text_input(
                    &t!("settings_sideloadly_completion_placeholder"),
                    &self.sideloadly_completion.0,
                )
                .secure(true)
                .on_input(|s| Message::UpdateSideloadlyCompletion(PatreonLoginInput(s)))
                .on_submit(Message::CompleteSideloadlyPatreonLogin)
                .padding(6),
            );
            options = options.push(
                row![
                    button(text(t!("settings_sideloadly_complete_login")))
                        .on_press_maybe(
                            (idle && !self.sideloadly_completion.0.trim().is_empty())
                                .then_some(Message::CompleteSideloadlyPatreonLogin)
                        )
                        .style(appearance::s_button),
                    button(text(t!("settings_sideloadly_cancel_login")))
                        .on_press_maybe(idle.then_some(Message::CancelSideloadlyPatreonLogin))
                        .style(appearance::s_button),
                ]
                .spacing(appearance::THEME_PADDING),
            );
        }
        options.into()
    }

    fn view_auto_start_toggle(&self, auto_start_enabled: bool) -> Element<'_, Message> {
        checkbox(auto_start_enabled)
            .label(t!("settings_launch_on_startup"))
            .on_toggle(Message::ToggleAutoStart)
            .into()
    }

    fn view_keyring_toggle(&self, store: &AccountStore) -> Element<'_, Message> {
        let available = plume_store::keyring_available();
        let toggle = checkbox(available && store.use_keyring())
            .label(t!("settings_use_keyring"))
            .on_toggle_maybe(available.then_some(Message::ToggleKeyring));
        if available {
            toggle.into()
        } else {
            row![
                toggle,
                text(t!("settings_keyring_unavailable")).size(appearance::THEME_FONT_SIZE - 1.0)
            ]
            .spacing(appearance::THEME_PADDING)
            .align_y(Alignment::Center)
            .into()
        }
    }

    fn view_language_picker<'a>(
        &'a self,
        selected_locale: &'a Option<String>,
    ) -> Element<'a, Message> {
        let mut codes: Vec<String> = rust_i18n::available_locales!()
            .into_iter()
            .map(|s| s.to_string())
            .collect();
        codes.sort();

        let mut choices: Vec<LocaleChoice> = Vec::with_capacity(codes.len() + 1);
        choices.push(LocaleChoice::system());
        choices.extend(codes.into_iter().map(LocaleChoice::explicit));

        let current = match selected_locale {
            None => LocaleChoice::system(),
            Some(code) => LocaleChoice::explicit(code.clone()),
        };

        let picker = pick_list(choices, Some(current), |choice: LocaleChoice| {
            Message::SelectLocale(choice.code().map(|s| s.to_string()))
        })
        .style(appearance::s_pick_list);

        column![text(t!("settings_language")), picker]
            .spacing(appearance::THEME_PADDING)
            .into()
    }

    fn view_appearance_and_language<'a>(
        &'a self,
        store: &'a AccountStore,
        selected_locale: &'a Option<String>,
    ) -> Element<'a, Message> {
        let appearance_picker = column![
            text(t!("settings_appearance")),
            pick_list(
                AppearanceMode::ALL.map(AppearanceChoice),
                Some(AppearanceChoice(store.appearance())),
                Message::SelectAppearance,
            )
            .style(appearance::s_pick_list),
        ]
        .spacing(appearance::THEME_PADDING);

        row![
            appearance_picker,
            self.view_language_picker(selected_locale)
        ]
        .spacing(appearance::THEME_PADDING * 2.0)
        .into()
    }
}

/// A bare Sideloadly anisette caps-token (e.g. `FzjicvV2PLvbwn14Bmj7s`): a short
/// token of URL-safe base62/base64 characters, with no scheme, dot or slash that
/// would make it a URL or hostname.
fn is_sideloadly_token(input: &str) -> bool {
    (16..=64).contains(&input.len())
        && input
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Expands a schemeless Sideloadly shortcut into a full v1 anisette server, so a
/// self-hosted instance is as easy to use as the public one. Accepts either a bare
/// caps-token (→ the default [`remote_anisette_v1::DEFAULT_SIDELOADLY_HOST`]) or a
/// `<host>/<token>` pair, where the host may also carry an `anisette/` path
/// (`ani.example.com/anisette/Fzji…`). Returns `None` when the input is neither.
fn parse_sideloadly_shortcut(input: &str) -> Option<AnisetteServer> {
    let (host, token) = match input.trim().split_once('/') {
        // `<host>/<token>`: a self-hosted instance. Require a dotted host so a stray
        // slash in something that isn't a shortcut doesn't look like one.
        Some((host, rest)) => {
            let token = rest.trim_matches('/');
            let token = token.strip_prefix("anisette/").unwrap_or(token);
            if !host.contains('.') {
                return None;
            }
            (host, token)
        }
        // A bare token: the default public instance.
        None => (remote_anisette_v1::DEFAULT_SIDELOADLY_HOST, input.trim()),
    };

    if !is_sideloadly_token(token) {
        return None;
    }
    let name = if host == remote_anisette_v1::DEFAULT_SIDELOADLY_HOST {
        t!("settings_anisette_sideloadly").to_string()
    } else {
        format!("{} ({host})", t!("settings_anisette_sideloadly"))
    };
    Some(AnisetteServer::new(
        name,
        remote_anisette_v1::sideloadly_anisette_url(host, token),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previewing_providers_and_servers_preserves_the_active_configuration() {
        let mut screen = SettingsScreen::new();
        let active = screen.anisette.clone();
        let _ = screen.update(
            Message::SelectRemoteProvider(RemoteProviderChoice(RemoteProvider::Custom)),
            None,
        );
        assert_eq!(screen.anisette, active);
        assert!(!screen.can_select_server());
        let _ = screen.update(
            Message::UpdateCustomAnisetteServer("https://preview.example".into()),
            None,
        );
        let _ = screen.update(Message::UseCustomAnisetteServer, None);
        assert_eq!(screen.anisette, active);
        assert_eq!(
            screen.pending_server.as_ref().unwrap().address,
            "https://preview.example"
        );
        assert!(screen.can_select_server());
        let _ = screen.update(
            Message::UpdateCustomAnisetteServer("https://edited.example".into()),
            None,
        );
        assert!(screen.pending_server.is_none());
        assert!(!screen.can_select_server());
    }

    #[test]
    fn select_server_is_disabled_for_the_active_endpoint_even_with_a_different_name() {
        let mut screen = SettingsScreen::new();
        screen.pending_server = Some(AnisetteServer::new(
            "Different name",
            &screen.anisette.remote_server.address,
        ));
        assert!(!screen.can_select_server());
        screen.pending_server = Some(AnisetteServer::new("Other", "https://other.example"));
        assert!(screen.can_select_server());
        screen.sideloadly_logging_in = true;
        assert!(!screen.can_select_server());
    }

    #[test]
    fn invalid_custom_url_cannot_leave_a_previous_server_selectable() {
        let mut screen = SettingsScreen::new();
        let _ = screen.update(Message::UpdateCustomAnisetteServer("https://".into()), None);
        let _ = screen.update(Message::UseCustomAnisetteServer, None);
        assert!(screen.pending_server.is_none());
        assert!(!screen.can_select_server());
        assert!(screen.anisette_status.is_some());
    }
}
