mod developer;
mod logs;
mod pairing;
mod source_manager;

pub(crate) use logs::Message as LogsMessage;
pub(crate) use pairing::Message as PairingMessage;

use std::collections::HashMap;

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
    ShowLogin,
    SelectAccount(usize),
    RemoveAccount(usize),
    ExportP12,
    SelectTeam(String, String),
    FetchTeams(String),
    TeamsLoaded(String, Vec<Team>),
    ToggleAutoStart(bool),
    SelectLocale(Option<String>),
    ResetAnisetteData,
    SelectAnisetteMode(AnisetteModeChoice),
    SetupLocalAnisette,
    LocalAnisetteReady(AnisetteMode, Result<(), String>),
    SelectAnisetteServer(AnisetteServer),
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
    Pairing,
    Developer,
    Sources,
    Logs,
}

impl SettingsTab {
    const ALL: [SettingsTab; 5] = [
        SettingsTab::General,
        SettingsTab::Pairing,
        SettingsTab::Developer,
        SettingsTab::Sources,
        SettingsTab::Logs,
    ];

    fn label(self) -> std::borrow::Cow<'static, str> {
        match self {
            SettingsTab::General => t!("settings_tab_general"),
            SettingsTab::Pairing => t!("settings_tab_pairing"),
            SettingsTab::Developer => t!("settings_tab_developer"),
            SettingsTab::Sources => t!("settings_tab_sources"),
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

impl std::fmt::Display for RemoteProviderChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self.0 {
            RemoteProvider::SideStore => t!("settings_anisette_sidestore"),
            RemoteProvider::Sideloadly => t!("settings_anisette_sideloadly"),
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
    teams: HashMap<String, Vec<Team>>,
    loading_teams: Option<String>,
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
    logs: logs::LogsTab,
}

impl SettingsScreen {
    pub fn new() -> Self {
        let anisette = AnisetteSettings::load_sync(&crate::defaults::get_data_path());
        let mut screen = Self {
            teams: HashMap::new(),
            loading_teams: None,
            anisette,
            anisette_servers: remote_servers::builtin_servers(),
            loading_anisette_servers: false,
            custom_anisette_server: String::new(),
            anisette_status: None,
            testing_anisette: false,
            sideloadly_logging_in: false,
            sideloadly_patreon_pending: false,
            sideloadly_completion: PatreonLoginInput::default(),
            tab: SettingsTab::General,
            pairing: pairing::PairingTab::new(),
            developer: developer::DeveloperTab::default(),
            sources: source_manager::SourcesTab::new(),
            logs: logs::LogsTab::default(),
        };
        screen.ensure_selected_server_listed();
        screen
    }

    fn ensure_selected_server_listed(&mut self) {
        let selected = &self.anisette.remote_server;
        if !self
            .anisette_servers
            .iter()
            .any(|s| s.address == selected.address)
        {
            self.anisette_servers.insert(0, selected.clone());
        }
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
        self.anisette.remote_server = server;
        self.anisette_status = None;
        self.ensure_selected_server_listed();
        self.save_anisette()
    }

    pub fn update(&mut self, message: Message, store: Option<&AccountStore>) -> Task<Message> {
        match message {
            Message::SelectTab(tab) => {
                self.tab = tab;
                match tab {
                    SettingsTab::Pairing => {
                        self.update(Message::Pairing(pairing::Message::Refresh), store)
                    }
                    SettingsTab::Developer => {
                        self.update(Message::Developer(developer::Message::Load), store)
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
            Message::FetchTeams(ref email) => {
                self.loading_teams = Some(email.clone());
                Task::none()
            }
            Message::TeamsLoaded(email, teams) => {
                self.teams.insert(email, teams);
                self.loading_teams = None;
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
                self.anisette.mode = mode;
                self.anisette_status = None;
                if self.save_anisette() && mode == AnisetteMode::Local {
                    return self.update(Message::SetupLocalAnisette, store);
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
                self.select_anisette_server(server);
                Task::none()
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
                Task::none()
            }
            Message::UseCustomAnisetteServer => {
                let input = self.custom_anisette_server.trim();
                let server = if input.starts_with("https://") || input.starts_with("http://") {
                    AnisetteServer::new(t!("settings_anisette_custom"), input)
                } else if let Some(server) = parse_sideloadly_shortcut(input) {
                    // A Sideloadly provider shortcut, optionally with a self-hosted host.
                    server
                } else {
                    self.anisette_status = Some(t!("settings_anisette_invalid_url").to_string());
                    return Task::none();
                };
                self.custom_anisette_server.clear();
                self.select_anisette_server(server);
                Task::none()
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
                        self.anisette_status = Some(
                            t!(
                                "settings_anisette_fastest_found",
                                name = server.name.clone()
                            )
                            .to_string(),
                        );
                        self.select_anisette_server(server);
                    }
                    None => {
                        self.anisette_status =
                            Some(t!("settings_anisette_none_responded").to_string());
                    }
                }
                Task::none()
            }
            Message::SelectRemoteProvider(RemoteProviderChoice(provider)) => {
                if self.sideloadly_logging_in {
                    return Task::none();
                }
                self.anisette.remote_provider = provider;
                self.anisette_status = None;
                self.save_anisette();
                if provider == RemoteProvider::Sideloadly {
                    if let Some(session) = &self.anisette.sideloadly_session {
                        self.select_anisette_server(session.server.clone());
                    }
                } else if self
                    .anisette
                    .sideloadly_session
                    .as_ref()
                    .is_some_and(|session| session.server == self.anisette.remote_server)
                {
                    self.select_anisette_server(remote_servers::builtin_servers().remove(0));
                }
                Task::none()
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
                        self.anisette.remote_provider = RemoteProvider::Sideloadly;
                        self.anisette.mode = AnisetteMode::Remote;
                        if self.select_anisette_server(server.clone()) {
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
                Task::none()
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
                Task::none()
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
                self.sideloadly_patreon_pending = false;
                self.sideloadly_completion = PatreonLoginInput::default();
                self.ensure_selected_server_listed();
                if self.save_anisette() {
                    self.anisette_status = Some(t!("settings_sideloadly_logged_out").to_string());
                }
                Task::none()
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
        let tabs = SettingsTab::ALL.iter().fold(
            row![].spacing(appearance::THEME_PADDING / 2.0),
            |tabs, &tab| {
                let style = if tab == self.tab {
                    appearance::p_button
                } else {
                    appearance::s_button
                };
                tabs.push(
                    button(text(tab.label()).align_x(Alignment::Center))
                        .on_press(Message::SelectTab(tab))
                        .style(style)
                        .width(Fill),
                )
            },
        );

        let remote_pairing = account_store.as_ref().is_some_and(|s| s.remote_pairing());
        let account = account_store.as_ref().and_then(|s| s.selected_account());
        let body = match self.tab {
            SettingsTab::General => self.view_general(account_store, selected_locale),
            SettingsTab::Pairing => self
                .pairing
                .view(remote_pairing, selected_device, devices)
                .map(Message::Pairing),
            SettingsTab::Developer => self.developer.view(account).map(Message::Developer),
            SettingsTab::Sources => self.sources.view().map(Message::Sources),
            SettingsTab::Logs => self.logs.view().map(Message::Logs),
        };

        column![tabs, body]
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

        let mut accounts: Vec<_> = store.accounts().iter().collect();
        accounts.sort_by_key(|(email, _)| *email);

        let selected_index = store
            .selected_account()
            .and_then(|acc| accounts.iter().position(|(e, _)| *e == acc.email()));

        let mut content = column![].spacing(appearance::THEME_PADDING);

        if !accounts.is_empty() {
            let account_list = accounts.iter().enumerate().fold(
                column![].spacing(appearance::THEME_PADDING),
                |content, (index, (email, account))| {
                    let marker = if Some(index) == selected_index {
                        "[✓] "
                    } else {
                        "[ ] "
                    };
                    let style = if Some(index) == selected_index {
                        appearance::p_button
                    } else {
                        appearance::s_button
                    };

                    let account_button = button(
                        text(format!("{}{}", marker, account.email()))
                            .size(appearance::THEME_FONT_SIZE)
                            .align_x(Alignment::Start),
                    )
                    .on_press(Message::SelectAccount(index))
                    .style(style)
                    .width(Fill);

                    let mut account_row = row![account_button].spacing(appearance::THEME_PADDING);

                    if Some(index) == selected_index {
                        let team_id = account.team_id();
                        let is_loading = self.loading_teams.as_ref() == Some(email);
                        let teams = self.teams.get(*email).cloned().unwrap_or_default();

                        let current_team = if !team_id.is_empty() {
                            teams.iter().find(|t| t.id == *team_id).cloned()
                        } else {
                            None
                        };

                        let placeholder = if is_loading {
                            t!("settings_select_teams").to_string()
                        } else if !team_id.is_empty() {
                            team_id.to_string()
                        } else {
                            t!("settings_loading_teams").to_string()
                        };

                        let email_owned = email.to_string();

                        let team_pick = pick_list(teams, current_team, move |selected: Team| {
                            Message::SelectTeam(email_owned.clone(), selected.id)
                        })
                        .placeholder(placeholder)
                        .on_open(Message::FetchTeams(email.to_string()))
                        .style(appearance::s_pick_list)
                        .width(Fill);

                        account_row = account_row.push(team_pick);
                    }

                    content.push(account_row)
                },
            );

            content = content.push(container(scrollable(account_list)).height(Fill).style(
                |theme: &iced::Theme| container::Style {
                    border: iced::Border {
                        width: 1.0,
                        color: theme.palette().background.scale_alpha(0.5),
                        radius: appearance::THEME_CORNER_RADIUS.into(),
                    },
                    ..Default::default()
                },
            ));
        } else {
            content = content.push(text(t!("settings_no_accounts_yet")));
        }

        let auto_start_enabled = crate::startup::auto_start_enabled();
        content = content.push(self.view_auto_start_toggle(auto_start_enabled));
        content = content.push(self.view_keyring_toggle(store));
        content = content.push(self.view_appearance_and_language(store, selected_locale));
        content = content.push(self.view_anisette());
        content = content.push(self.view_account_buttons(selected_index));

        content.into()
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

        if mode == AnisetteMode::Remote {
            let provider = self.anisette.remote_provider;
            let provider_picker = pick_list(
                RemoteProvider::ALL.map(RemoteProviderChoice),
                Some(RemoteProviderChoice(provider)),
                Message::SelectRemoteProvider,
            )
            .style(appearance::s_pick_list);

            if provider == RemoteProvider::SideStore {
                let selected = self
                    .anisette_servers
                    .iter()
                    .find(|s| s.address == self.anisette.remote_server.address)
                    .cloned();
                let placeholder = if self.loading_anisette_servers {
                    t!("settings_anisette_loading_servers").to_string()
                } else {
                    self.anisette.remote_server.to_string()
                };
                section = section.push(
                    row![
                        provider_picker,
                        pick_list(
                            self.anisette_servers.clone(),
                            selected,
                            Message::SelectAnisetteServer,
                        )
                        .placeholder(placeholder)
                        .on_open(Message::FetchAnisetteServers)
                        .style(appearance::s_pick_list)
                        .width(Fill),
                    ]
                    .spacing(appearance::THEME_PADDING)
                    .align_y(Alignment::Center),
                );

                let custom_input = text_input(
                    &t!("settings_anisette_custom_placeholder"),
                    &self.custom_anisette_server,
                )
                .on_input(Message::UpdateCustomAnisetteServer)
                .on_submit(Message::UseCustomAnisetteServer)
                .padding(6)
                .width(Fill);
                let use_custom = button(text(t!("settings_anisette_use_custom")))
                    .on_press_maybe(
                        (!self.custom_anisette_server.trim().is_empty())
                            .then_some(Message::UseCustomAnisetteServer),
                    )
                    .style(appearance::s_button);
                section = section.push(
                    row![custom_input, use_custom]
                        .spacing(appearance::THEME_PADDING)
                        .align_y(Alignment::Center),
                );
                section = section.push(
                    button(text(t!("settings_anisette_find_fastest")))
                        .on_press_maybe(
                            (!self.testing_anisette).then_some(Message::FindFastestAnisetteServer),
                        )
                        .style(appearance::s_button),
                );
            } else {
                section = section.push(provider_picker);
            }
            section = section.push(self.view_sideloadly_options());
        } else {
            let description = if mode == AnisetteMode::Local {
                t!("settings_anisette_local_desc")
            } else {
                t!("settings_anisette_fake_desc")
            };
            section = section.push(text(description).size(appearance::THEME_FONT_SIZE - 1.0));
            section = section.push(
                button(text(t!("settings_anisette_local_setup")))
                    .on_press_maybe((!self.testing_anisette).then_some(Message::SetupLocalAnisette))
                    .style(appearance::s_button),
            );
        }

        let test_button = button(text(t!("settings_anisette_test")))
            .on_press_maybe((!self.testing_anisette).then_some(Message::TestAnisette))
            .style(appearance::s_button);
        let reset_button = button(appearance::icon_text(
            appearance::MINUS,
            t!("settings_reset_adi"),
            None,
        ))
        .on_press_maybe((!self.testing_anisette).then_some(Message::ResetAnisetteData))
        .style(appearance::s_button);

        let mut actions = row![test_button, reset_button]
            .spacing(appearance::THEME_PADDING)
            .align_y(Alignment::Center);
        if let Some(status) = &self.anisette_status {
            actions = actions.push(text(status).size(appearance::THEME_FONT_SIZE - 1.0));
        }

        section.push(actions).into()
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
            text(t!("settings_anisette_sideloadly")),
            text(account).size(appearance::THEME_FONT_SIZE - 1.0),
            row![
                button(text(t!("settings_sideloadly_login_anonymously")))
                    .on_press_maybe(idle.then_some(Message::SideloadlyLogin))
                    .style(appearance::s_button),
                button(text(t!("settings_sideloadly_refresh_device_token")))
                    .on_press_maybe(
                        (idle && signed_in).then_some(Message::SideloadlyRefreshDeviceToken)
                    )
                    .style(appearance::s_button),
            ]
            .spacing(appearance::THEME_PADDING),
            row![
                button(text(t!("settings_sideloadly_login_patreon")))
                    .on_press_maybe(idle.then_some(Message::SideloadlyPatreonLogin))
                    .style(appearance::s_button),
                button(text(t!("settings_sideloadly_logout")))
                    .on_press_maybe(
                        (idle && (signed_in || self.sideloadly_patreon_pending))
                            .then_some(Message::SideloadlyLogout)
                    )
                    .style(appearance::s_button),
                button(text(t!("settings_sideloadly_account_status")))
                    .on_press_maybe(idle.then_some(Message::SideloadlyAccountStatus))
                    .style(appearance::s_button),
            ]
            .spacing(appearance::THEME_PADDING),
        ]
        .spacing(appearance::THEME_PADDING);
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

    fn view_account_buttons(&self, selected_index: Option<usize>) -> Element<'_, Message> {
        let mut buttons = row![
            button(appearance::icon_text(
                appearance::PLUS,
                t!("settings_add_account"),
                None
            ))
            .on_press(Message::ShowLogin)
            .style(appearance::s_button)
        ]
        .spacing(appearance::THEME_PADDING);

        if let Some(index) = selected_index {
            buttons = buttons
                .push(
                    button(appearance::icon_text(
                        appearance::MINUS,
                        t!("settings_remove_account"),
                        None,
                    ))
                    .on_press(Message::RemoveAccount(index))
                    .style(appearance::s_button),
                )
                .push(
                    button(appearance::icon_text(
                        appearance::SHARE,
                        t!("settings_export_p12"),
                        None,
                    ))
                    .on_press(Message::ExportP12)
                    .style(appearance::s_button),
                );
        }

        buttons.align_y(Alignment::Center).into()
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
