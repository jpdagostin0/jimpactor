mod device_lock;
pub(crate) mod general;
mod package;
mod progress;
mod quick_install;
pub(crate) mod settings;
pub(crate) mod utilties;
mod windows;

use std::collections::{HashSet, VecDeque};

use iced::Length::Fill;
use iced::event::{self, Event, Status};
use iced::keyboard::{self, Key, key};
use iced::widget::operation::{focus_next, focus_previous};
use iced::widget::{Stack, button, column, container, mouse_area, opaque, pick_list, row, text};
use iced::window;
use iced::{Alignment, Element, Subscription, Task};

use plume_store::{AccountStore, AppearanceMode};
use plume_utils::{Device, SignerOptions};

use crate::subscriptions;
use crate::tray::ImpactorTray;
use crate::{appearance, defaults};
use windows::login_window;

const SIDEBAR_WIDTH: f32 = 180.0;
const SETTINGS_POPUP_WIDTH: f32 = 720.0;
const SETTINGS_POPUP_HEIGHT: f32 = 480.0;

static REFRESH_DAEMON_DEVICES: std::sync::OnceLock<crate::refresh::ConnectedDevices> =
    std::sync::OnceLock::new();

pub fn set_refresh_daemon_devices(devices: crate::refresh::ConnectedDevices) {
    let _ = REFRESH_DAEMON_DEVICES.set(devices);
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum Message {
    // Navigation
    NavigateToScreen(ImpactorScreenType),
    /// Shows one of the selected device's sections in the sidebar.
    SelectDeviceTab(utilties::UtilitiesTab),
    OpenSettings,
    CloseSettings,
    NextScreen,
    PreviousScreen,
    TabPressed {
        shift: bool,
    },

    // Device management
    ComboBoxSelected(String),
    /// Locks installs to the device chosen in the picker (device locking only).
    SelectDevice,
    DeviceConnected(Device),
    DeviceDisconnected(u32),

    // Tray
    TrayMenuClicked(crate::tray::TrayAction),
    TrayMenuSelected(tray_icon::menu::MenuId),
    TrayIconClicked,
    #[cfg(target_os = "macos")]
    MacOsActivationTick,

    // Refresh operations
    RefreshAppNow {
        udid: String,
        app_path: String,
    },
    ForgetApp {
        udid: String,
        app_path: String,
    },
    UpdateTrayMenu,

    // Window management
    RelaunchRequested,
    RestartRequested,
    ShowWindow,
    HideWindow,
    WindowCloseRequested(window::Id),
    Quit,

    // Login window
    LoginWindow(window::Id, login_window::Message),

    // Screen-specific messages
    MainScreen(general::Message),
    UtilitiesScreen(utilties::Message),
    SettingsScreen(settings::Message),
    InstallerScreen(package::Message),
    ProgressScreen(progress::Message),
    CertificateResetRequested(crate::certificate_reset::ConfirmationRequest),
    ConfirmCertificateReset,
    CancelCertificateReset,

    // Installation
    StartInstallation,

    // Appearance
    SystemThemeChanged(iced::theme::Mode),
}

pub struct Impactor {
    current_screen: ImpactorScreen,
    /// The selected device's sections. Kept while other screens show so syslog keeps streaming.
    utilities: Box<utilties::UtilitiesScreen>,
    /// The settings popup, while it is open.
    settings: Option<Box<settings::SettingsScreen>>,
    installer_screen: Option<Box<package::PackageScreen>>,
    devices: Vec<Device>,
    /// Where installs go. With device locking, this is the locked device while it is connected.
    selected_device: Option<Device>,
    /// The device shown in the picker while device locking is on; Select locks to it.
    device_candidate: Option<Device>,
    device_lock: Option<device_lock::DeviceLock>,
    tray: Option<ImpactorTray>,
    main_window: Option<window::Id>,
    account_store: Option<AccountStore>,
    login_windows: std::collections::HashMap<window::Id, login_window::LoginWindow>,
    pending_installation: bool,
    certificate_reset_queue: VecDeque<crate::certificate_reset::ConfirmationRequest>,
    selected_locale: Option<String>,
    quick_install: quick_install::QuickInstall,
    pending_source_install: Option<(crate::sources::SourceApp, crate::sources::AppVersion)>,
    system_theme: iced::theme::Mode,
    /// (UDID, bundle ID, version) of app updates already announced.
    announced_updates: HashSet<(String, String, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ImpactorScreenType {
    Main,
    Installer,
    Progress,
}

enum ImpactorScreen {
    Main(general::GeneralScreen),
    Utilities,
    Installer(Box<package::PackageScreen>),
    Progress(progress::ProgressScreen),
}

impl Impactor {
    pub fn new() -> (Self, Task<Message>) {
        let mut tray = ImpactorTray::new();
        let mut store = Self::init_account_store_sync();
        if let Err(err) = store.migrate_secrets_sync() {
            log::error!("Failed to move account tokens into the keyring: {err}");
        }
        tray.update_refresh_apps(&store);
        let selected_locale = store.locale().map(str::to_string);
        let remote_pairing = store.remote_pairing();
        let device_lock = store
            .lock_device()
            .then(|| {
                store
                    .locked_device()
                    .map(device_lock::DeviceLock::from_saved)
            })
            .flatten();
        Self::apply_tailscale(store.tailscale(), None, true);
        if let Some(devices) = REFRESH_DAEMON_DEVICES.get() {
            crate::jit_server::apply(store.jit_server(), devices.clone());
        }
        if let Some(code) = &selected_locale {
            rust_i18n::set_locale(code);
        }
        let start_in_tray = crate::startup::start_in_tray_from_args();
        let (main_window, open_task) = if start_in_tray {
            (None, Task::none())
        } else {
            let (id, open_task) = window::open(defaults::default_window_settings());
            (Some(id), open_task.discard())
        };
        crate::macos_app::set_main_window_visible(true);
        crate::macos_app::reset_activation_state();

        (
            Self {
                current_screen: ImpactorScreen::Main(general::GeneralScreen::new()),
                utilities: Box::new(utilties::UtilitiesScreen::new(None, remote_pairing)),
                settings: None,
                installer_screen: None,
                devices: Vec::new(),
                selected_device: None,
                device_candidate: None,
                device_lock,
                tray: Some(tray),
                main_window,
                account_store: Some(store),
                login_windows: std::collections::HashMap::new(),
                pending_installation: false,
                certificate_reset_queue: VecDeque::new(),
                selected_locale,
                quick_install: quick_install::QuickInstall::default(),
                pending_source_install: None,
                system_theme: iced::theme::Mode::None,
                announced_updates: HashSet::new(),
            },
            Task::batch([
                open_task,
                iced::system::theme().map(Message::SystemThemeChanged),
                Self::quick_install_task(quick_install::Message::Load),
            ]),
        )
    }

    fn init_account_store_sync() -> AccountStore {
        let path = defaults::get_data_path().join("accounts.json");
        AccountStore::load_sync(&Some(path)).unwrap_or_default()
    }

    pub fn theme(&self, _window: window::Id) -> iced::Theme {
        let mode = self
            .account_store
            .as_ref()
            .map(|s| s.appearance())
            .unwrap_or_default();
        let theme = match mode {
            AppearanceMode::Light => appearance::PlumeTheme::Light,
            AppearanceMode::Dark => appearance::PlumeTheme::Dark,
            AppearanceMode::System => match self.system_theme {
                iced::theme::Mode::Light => appearance::PlumeTheme::Light,
                _ => appearance::PlumeTheme::Dark,
            },
        };
        theme.to_iced_theme()
    }

    fn quick_install_task(msg: quick_install::Message) -> Task<Message> {
        Task::done(Message::MainScreen(general::Message::QuickInstall(msg)))
    }

    fn remote_pairing(&self) -> bool {
        self.account_store
            .as_ref()
            .is_some_and(|s| s.remote_pairing())
    }

    fn set_remote_pairing(&mut self, enabled: bool) {
        if let Some(store) = &mut self.account_store
            && let Err(err) = store.set_remote_pairing_sync(enabled)
        {
            log::error!("Failed to save remote pairing setting: {err}");
        }
    }

    fn lock_enabled(&self) -> bool {
        self.account_store.as_ref().is_some_and(|s| s.lock_device())
    }

    fn save_device_lock(&mut self) {
        let saved = self
            .device_lock
            .as_ref()
            .map(device_lock::DeviceLock::to_saved);
        if let Some(store) = &mut self.account_store
            && let Err(err) = store.set_locked_device_sync(saved)
        {
            log::error!("Failed to save the locked device: {err}");
        }
    }

    /// Starts, restarts or stops the embedded tailnet node. The node restarts only when its
    /// connection settings change; device list edits just update discovery.
    fn apply_tailscale(
        settings: &plume_store::TailscaleSettings,
        auth_key: Option<String>,
        restart: bool,
    ) {
        plume_utils::set_tailnet_devices(settings.devices.clone());
        if !restart {
            return;
        }
        if settings.enabled {
            plume_utils::start_tailnet(plume_utils::TailnetConfig {
                hostname: settings.hostname.clone(),
                control_url: settings.control_url.clone(),
                key_file: defaults::get_data_path().join("tailscale_keys.json"),
                auth_key,
            });
        } else {
            plume_utils::stop_tailnet(None);
        }
    }

    /// Points the device sections at the current target, reloading the open section.
    fn sync_utilities(&mut self) -> Task<Message> {
        if self.utilities.targets(self.selected_device.as_ref()) {
            return Task::none();
        }
        let tab = self.utilities.tab();
        self.utilities = Box::new(utilties::UtilitiesScreen::new(
            self.selected_device.clone(),
            self.remote_pairing(),
        ));
        self.utilities.set_source_apps(self.quick_install.apps());
        let select = Task::done(Message::UtilitiesScreen(utilties::Message::SelectTab(tab)));
        // Load the app list right away so app updates are found as the device connects.
        Task::batch([select, self.refresh_apps()])
    }

    /// Tells the user about app updates that have not been mentioned yet.
    fn announce_updates(&mut self) {
        let Some(device) = self.utilities.device() else {
            return;
        };
        let new: Vec<String> = self
            .utilities
            .updates()
            .iter()
            .filter(|update| {
                self.announced_updates.insert((
                    device.udid.clone(),
                    update.bundle_id.clone(),
                    update.version.version.clone(),
                ))
            })
            .map(|update| update.name.clone())
            .collect();
        if new.is_empty() {
            return;
        }
        let body = rust_i18n::t!(
            "app_updates_available",
            apps = new.join(", "),
            device = device.name.as_str()
        )
        .into_owned();
        // Showing a notification can block; never do that on the UI thread.
        std::thread::spawn(move || {
            notify_rust::Notification::new()
                .summary(crate::APP_NAME)
                .body(&body)
                .show()
                .ok();
        });
    }

    /// Lines shown above the install options, comparing `package` with the device and sources.
    fn install_notes(&self, package: &plume_utils::Package) -> Vec<String> {
        use plume_utils::PlistInfoTrait;

        let Some(bundle_id) = package.get_bundle_identifier() else {
            return Vec::new();
        };
        let name = package.get_name();
        let version = package.get_version();
        let check = plume_utils::updates::check_install(
            &bundle_id,
            name.as_deref(),
            version.as_deref(),
            self.utilities.installed_apps(),
            self.quick_install.apps(),
        );

        let mut notes = Vec::new();
        if let Some(installed) = &check.installed {
            let key = if check.downgrade {
                "install_note_downgrade"
            } else {
                "install_note_installed"
            };
            notes.push(rust_i18n::t!(key, version = installed.as_str()).into_owned());
        }
        if let Some((version, source)) = &check.newer {
            notes.push(
                rust_i18n::t!(
                    "install_note_newer",
                    version = version.as_str(),
                    source = source.as_str()
                )
                .into_owned(),
            );
        }
        notes
    }

    fn refresh_apps(&self) -> Task<Message> {
        if self.selected_device.is_none() {
            return Task::none();
        }
        Task::done(Message::UtilitiesScreen(utilties::Message::RefreshApps(
            self.utilities.rppairing_enabled,
        )))
    }

    fn respond_to_next_certificate_reset(&mut self, accepted: bool) {
        if let Some(request) = self.certificate_reset_queue.pop_front() {
            request.respond(accepted);
        }
    }

    fn cancel_pending_certificate_resets(&mut self) {
        while let Some(request) = self.certificate_reset_queue.pop_front() {
            request.respond(false);
        }
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::SelectDevice => {
                let Some(candidate) = self.device_candidate.clone() else {
                    return Task::none();
                };
                if !self.lock_enabled() {
                    return Task::none();
                }
                log::info!("Locked installs to {candidate}");
                let lock = device_lock::DeviceLock::new(&candidate);
                self.selected_device = lock.select(&self.devices);
                self.device_lock = Some(lock);
                self.save_device_lock();
                self.sync_utilities()
            }
            Message::ComboBoxSelected(value) => {
                let device = self
                    .devices
                    .iter()
                    .find(|d| d.to_string() == value)
                    .cloned();
                if self.lock_enabled() {
                    // Picking only previews a device; Select makes it the install target.
                    self.device_candidate = device;
                    return Task::none();
                }
                self.selected_device = device;
                self.sync_utilities()
            }
            Message::DeviceConnected(device) => {
                if let Some(existing) = self
                    .devices
                    .iter_mut()
                    .find(|d| d.device_id == device.device_id)
                {
                    *existing = device.clone();
                    if self.selected_device.as_ref().map(|d| d.device_id) == Some(device.device_id)
                    {
                        self.selected_device = Some(device.clone());
                    }
                } else {
                    self.devices.push(device.clone());

                    if self.selected_device.is_none() && device.device_id != u32::MAX {
                        self.selected_device = Some(device.clone());
                    }
                }

                if self.lock_enabled() {
                    self.selected_device = self
                        .device_lock
                        .as_ref()
                        .and_then(|lock| lock.select(&self.devices));
                    if let Some(candidate) = &self.device_candidate
                        && candidate.device_id == device.device_id
                    {
                        self.device_candidate = Some(device.clone());
                    }
                    if self.device_candidate.is_none() {
                        self.device_candidate = self
                            .selected_device
                            .clone()
                            .or_else(|| Some(device.clone()).filter(|d| d.device_id != u32::MAX));
                    }
                }
                if let Some(daemon_devices) = REFRESH_DAEMON_DEVICES.get()
                    && let Ok(mut devices) = daemon_devices.lock()
                {
                    devices.insert(device.udid.clone(), device.clone());
                }

                self.sync_utilities()
            }
            Message::DeviceDisconnected(id) => {
                let udid = self
                    .devices
                    .iter()
                    .find(|d| d.device_id == id)
                    .map(|d| d.udid.clone());

                self.devices.retain(|d| d.device_id != id);

                if self.lock_enabled() {
                    self.selected_device = self
                        .device_lock
                        .as_ref()
                        .and_then(|lock| lock.select(&self.devices));
                    if self.device_candidate.as_ref().map(|d| d.device_id) == Some(id) {
                        self.device_candidate = self
                            .devices
                            .iter()
                            .find(|device| Some(&device.udid) == udid.as_ref())
                            .cloned();
                    }
                } else if self.selected_device.as_ref().map(|d| d.device_id) == Some(id) {
                    self.selected_device = self
                        .devices
                        .iter()
                        .find(|device| Some(&device.udid) == udid.as_ref())
                        .or_else(|| self.devices.first())
                        .cloned();
                }

                if let (Some(udid), Some(daemon_devices)) = (udid, REFRESH_DAEMON_DEVICES.get())
                    && let Ok(mut devices) = daemon_devices.lock()
                {
                    if let Some(remaining) = self.devices.iter().find(|device| device.udid == udid)
                    {
                        devices.insert(udid, remaining.clone());
                    } else {
                        devices.remove(&udid);
                    }
                }

                self.sync_utilities()
            }
            Message::NavigateToScreen(screen_type) => {
                self.navigate_to_screen(screen_type);
                Task::none()
            }
            Message::SelectDeviceTab(tab) => {
                let reopened = matches!(self.current_screen, ImpactorScreen::Utilities)
                    && self.utilities.tab() == tab;
                self.current_screen = ImpactorScreen::Utilities;
                let select =
                    Task::done(Message::UtilitiesScreen(utilties::Message::SelectTab(tab)));
                if tab == utilties::UtilitiesTab::Apps && !reopened {
                    Task::batch([select, self.refresh_apps()])
                } else {
                    select
                }
            }
            Message::OpenSettings => {
                self.settings = Some(Box::new(settings::SettingsScreen::new()));
                Task::done(Message::SettingsScreen(
                    settings::Message::ProbeRemoteServer,
                ))
            }
            Message::CloseSettings => {
                self.settings = None;
                Task::none()
            }

            Message::TabPressed { shift } => {
                if shift {
                    focus_previous()
                } else {
                    focus_next()
                }
            }
            Message::NextScreen => {
                let next_screen = match self.current_screen {
                    ImpactorScreen::Main(_) => ImpactorScreenType::Installer,
                    ImpactorScreen::Utilities => return Task::none(),
                    ImpactorScreen::Installer(_) => ImpactorScreenType::Progress,
                    ImpactorScreen::Progress(_) => return Task::none(),
                };

                self.navigate_to_screen(next_screen);
                Task::none()
            }
            Message::PreviousScreen => match &self.current_screen {
                ImpactorScreen::Main(_) => Task::none(),
                ImpactorScreen::Utilities => {
                    self.navigate_to_screen(ImpactorScreenType::Main);
                    Task::none()
                }
                ImpactorScreen::Installer(_) => {
                    self.navigate_to_screen(ImpactorScreenType::Main);
                    Task::none()
                }
                ImpactorScreen::Progress(_) => {
                    if let Some(installer) = self.installer_screen.take() {
                        self.current_screen = ImpactorScreen::Installer(installer);
                    } else {
                        self.navigate_to_screen(ImpactorScreenType::Main);
                    }
                    Task::none()
                }
            },
            Message::TrayIconClicked => Task::done(Message::ShowWindow),
            Message::TrayMenuSelected(id) => self
                .tray
                .as_ref()
                .and_then(|tray| tray.action(&id))
                .map(|action| Task::done(Message::TrayMenuClicked(action)))
                .unwrap_or_else(Task::none),
            Message::TrayMenuClicked(action) => match action {
                crate::tray::TrayAction::Show => Task::done(Message::ShowWindow),
                crate::tray::TrayAction::Quit => Task::done(Message::Quit),
                crate::tray::TrayAction::RefreshApp { udid, app_path } => {
                    Task::done(Message::RefreshAppNow {
                        udid: udid.clone(),
                        app_path: app_path.clone(),
                    })
                }
                crate::tray::TrayAction::ForgetApp { udid, app_path } => {
                    Task::done(Message::ForgetApp {
                        udid: udid.clone(),
                        app_path: app_path.clone(),
                    })
                }
            },
            #[cfg(target_os = "macos")]
            Message::MacOsActivationTick => {
                if self.main_window.is_none() && crate::macos_app::activation_reopen_requested() {
                    Task::done(Message::RelaunchRequested)
                } else {
                    Task::none()
                }
            }
            Message::RestartRequested => {
                let installing = matches!(&self.current_screen, ImpactorScreen::Progress(screen) if screen.is_installing);
                if installing || crate::refresh::has_active_refreshes() {
                    log::warn!("Finish the current installation or refresh before restarting");
                    Task::done(Message::RelaunchRequested)
                } else {
                    Task::done(Message::Quit)
                }
            }
            Message::RelaunchRequested => {
                if self.main_window.is_none() {
                    Task::done(Message::ShowWindow)
                } else {
                    Task::none()
                }
            }
            Message::CertificateResetRequested(request) => {
                self.certificate_reset_queue.push_back(request);
                if self.main_window.is_none() {
                    Task::done(Message::ShowWindow)
                } else {
                    Task::none()
                }
            }
            Message::ConfirmCertificateReset => {
                self.respond_to_next_certificate_reset(true);
                Task::none()
            }
            Message::CancelCertificateReset => {
                self.respond_to_next_certificate_reset(false);
                Task::none()
            }
            Message::ShowWindow => {
                crate::macos_app::set_main_window_visible(true);
                if let Some(id) = self.main_window {
                    window::gain_focus(id)
                } else {
                    let (id, open_task) = window::open(defaults::default_window_settings());
                    self.main_window = Some(id);
                    open_task.discard()
                }
            }
            Message::WindowCloseRequested(id) => {
                if self.login_windows.contains_key(&id) {
                    Task::done(Message::LoginWindow(id, login_window::Message::LoginCancel))
                } else if self.main_window == Some(id) {
                    if self
                        .account_store
                        .as_ref()
                        .is_none_or(|s| s.close_to_tray())
                    {
                        Task::done(Message::HideWindow)
                    } else {
                        // Quits, unless an install or refresh is running (then it stays in the tray).
                        Task::done(Message::RestartRequested)
                    }
                } else {
                    Task::none()
                }
            }
            Message::HideWindow => {
                if let Some(id) = self.main_window {
                    self.cancel_pending_certificate_resets();
                    self.main_window = None;
                    crate::macos_app::set_main_window_visible(false);
                    window::close(id)
                } else {
                    Task::none()
                }
            }
            Message::Quit => {
                self.tray.take();
                std::process::exit(0);
            }
            Message::LoginWindow(id, msg) => {
                if !self.login_windows.contains_key(&id) {
                    return Task::none();
                }
                if let login_window::Message::LoginSuccess(account) = &msg {
                    let mut store = self
                        .account_store
                        .clone()
                        .unwrap_or_else(Self::init_account_store_sync);
                    if let Err(error) = store.accounts_add_sync(account.clone()) {
                        return self
                            .login_windows
                            .get_mut(&id)
                            .unwrap()
                            .update(login_window::Message::LoginFailed(format!(
                                "Could not save your account: {error}"
                            )))
                            .map(move |msg| Message::LoginWindow(id, msg));
                    }
                    self.account_store = Some(store);
                }
                if let Some(login_window) = self.login_windows.get_mut(&id) {
                    let task = login_window.update(msg.clone());

                    if matches!(
                        msg,
                        login_window::Message::LoginSuccess(_)
                            | login_window::Message::LoginCancel
                            | login_window::Message::TwoFactorCancel
                    ) {
                        self.login_windows.remove(&id);
                        let settings_task = if let Some(screen) = &mut self.settings {
                            if let login_window::Message::LoginSuccess(account) = &msg {
                                screen.account_changed(account.email());
                                screen.tab = settings::SettingsTab::Account;
                                screen
                                    .update(
                                        settings::Message::FetchTeams(account.email().clone()),
                                        self.account_store.as_ref(),
                                    )
                                    .map(Message::SettingsScreen)
                            } else {
                                Task::none()
                            }
                        } else {
                            Task::none()
                        };

                        if let Some((app, version)) = self.pending_source_install.take()
                            && matches!(msg, login_window::Message::LoginSuccess(_))
                        {
                            return Task::batch([
                                window::close(id),
                                self.start_source_install(app, version),
                            ]);
                        }

                        if self.pending_installation {
                            self.pending_installation = false;
                            if matches!(msg, login_window::Message::LoginSuccess(_)) {
                                return Task::batch(vec![
                                    window::close(id),
                                    Task::done(Message::InstallerScreen(
                                        package::Message::RequestInstallation,
                                    )),
                                ]);
                            }
                        }

                        return Task::batch([window::close(id), settings_task]);
                    }

                    task.map(move |msg| Message::LoginWindow(id, msg))
                } else {
                    Task::none()
                }
            }
            Message::MainScreen(general::Message::QuickInstall(msg)) => match msg {
                quick_install::Message::Install(app, version) => {
                    self.request_source_install(app, version)
                }
                msg => {
                    let loaded = matches!(msg, quick_install::Message::Loaded(_));
                    let task = self
                        .quick_install
                        .update(msg)
                        .map(|m| Message::MainScreen(general::Message::QuickInstall(m)));
                    if loaded {
                        self.utilities.set_source_apps(self.quick_install.apps());
                        self.announce_updates();
                    }
                    task
                }
            },
            Message::MainScreen(msg) => {
                if let ImpactorScreen::Main(ref mut screen) = self.current_screen {
                    let task = screen.update(msg.clone()).map(Message::MainScreen);

                    if let general::Message::NavigateToInstaller(package) = msg {
                        let mut options = SignerOptions::default();
                        package.load_into_signer_options(&mut options);
                        let notes = self.install_notes(&package);
                        let mut installer = package::PackageScreen::new(Some(package), options);
                        installer.set_notes(notes);
                        installer.load_saved_profile();
                        self.current_screen = ImpactorScreen::Installer(Box::new(installer));
                    }

                    task
                } else {
                    Task::none()
                }
            }
            Message::UtilitiesScreen(utilties::Message::InstallUpdate(update)) => {
                self.request_source_install(update.app, update.version)
            }
            Message::UtilitiesScreen(msg) => {
                let task = self.utilities.update(msg).map(Message::UtilitiesScreen);
                self.announce_updates();
                task
            }
            Message::SettingsScreen(msg) => {
                if let Some(screen) = &mut self.settings {
                    screen.device = self.selected_device.clone();
                    match msg {
                        settings::Message::ShowLogin(email) => {
                            if let Some(id) = self.login_windows.keys().next().copied() {
                                return window::gain_focus(id);
                            }
                            let (login_window, task) =
                                login_window::LoginWindow::with_email(email.unwrap_or_default());
                            let id = login_window.window_id().unwrap();
                            self.login_windows.insert(id, login_window);
                            task.map(move |msg| Message::LoginWindow(id, msg))
                        }
                        settings::Message::SelectAccount(email) => {
                            if let Some(store) = &mut self.account_store
                                && let Err(error) = store.account_select_sync(&email)
                            {
                                log::error!("Failed to select account: {error}");
                                return Task::none();
                            }
                            screen
                                .update(
                                    settings::Message::FetchTeams(email),
                                    self.account_store.as_ref(),
                                )
                                .map(Message::SettingsScreen)
                        }
                        settings::Message::RemoveAccount(email) => {
                            if let Some(store) = &mut self.account_store
                                && let Err(error) = store.accounts_remove_sync(&email)
                            {
                                log::error!("Failed to remove account: {error}");
                            }
                            screen.account_changed(&email);
                            Task::none()
                        }
                        settings::Message::ExportP12 => {
                            if let Some(account) = self
                                .account_store
                                .as_ref()
                                .and_then(|s| s.selected_account().cloned())
                            {
                                let (tx, rx) = futures::channel::oneshot::channel();
                                std::thread::spawn(move || {
                                    let result = tokio::runtime::Builder::new_current_thread()
                                        .enable_all()
                                        .build()
                                        .map_err(|e| e.to_string())
                                        .and_then(|runtime| {
                                            runtime.block_on(
                                                crate::subscriptions::export_certificate(account),
                                            )
                                        });
                                    let _ = tx.send(result);
                                });
                                return Task::perform(
                                    async move {
                                        rx.await.unwrap_or_else(|_| Err("Certificate export worker stopped unexpectedly".into()))
                                    },
                                    |result| {
                                        Message::SettingsScreen(
                                            settings::Message::CertificateExported(result),
                                        )
                                    },
                                );
                            }

                            Task::none()
                        }
                        settings::Message::ToggleAutoStart(enabled) => {
                            if let Err(err) = crate::startup::set_auto_start_enabled(enabled) {
                                log::error!("Failed to update auto-start: {err}");
                            }
                            Task::none()
                        }
                        settings::Message::SelectTeam(ref email, ref team_id) => {
                            if let Some(store) = &mut self.account_store {
                                if let Err(e) =
                                    store.update_account_team_sync(email, team_id.clone())
                                {
                                    log::error!("Failed to update team: {:?}", e);
                                } else {
                                    self.account_store = Some(Self::init_account_store_sync());
                                }
                            }
                            screen
                                .update(msg, self.account_store.as_ref())
                                .map(Message::SettingsScreen)
                        }
                        settings::Message::SelectAppearance(settings::AppearanceChoice(mode)) => {
                            if let Some(store) = &mut self.account_store
                                && let Err(err) = store.set_appearance_sync(mode)
                            {
                                log::error!("Failed to save appearance: {err}");
                            }
                            Task::none()
                        }
                        settings::Message::ToggleLockDevice(enabled) => {
                            if let Some(store) = &mut self.account_store
                                && let Err(err) = store.set_lock_device_sync(enabled)
                            {
                                log::error!("Failed to save device locking: {err}");
                            }
                            if enabled {
                                // Start locked to the current target so nothing changes until Select.
                                self.device_lock = self
                                    .selected_device
                                    .as_ref()
                                    .map(device_lock::DeviceLock::new);
                                self.device_candidate = self.selected_device.clone();
                            } else {
                                if self.selected_device.is_none() {
                                    self.selected_device = self
                                        .device_candidate
                                        .clone()
                                        .or_else(|| self.devices.first().cloned());
                                }
                                self.device_lock = None;
                                self.device_candidate = None;
                            }
                            self.save_device_lock();
                            self.sync_utilities()
                        }
                        settings::Message::ToggleCloseToTray(enabled) => {
                            if let Some(store) = &mut self.account_store
                                && let Err(err) = store.set_close_to_tray_sync(enabled)
                            {
                                log::error!("Failed to save close-to-tray: {err}");
                            }
                            Task::none()
                        }
                        settings::Message::Developer(settings::DeveloperMessage::KeyChanged(
                            key,
                        )) => {
                            if let Some(store) = &mut self.account_store
                                && let Err(err) = store.set_asc_key_sync(key)
                            {
                                log::error!("Failed to save the App Store Connect key: {err}");
                            }
                            screen
                                .update(
                                    settings::Message::Developer(settings::DeveloperMessage::Load),
                                    self.account_store.as_ref(),
                                )
                                .map(Message::SettingsScreen)
                        }
                        settings::Message::ToggleRefreshAlerts(enabled) => {
                            if let Some(store) = &mut self.account_store
                                && let Err(err) = store.set_refresh_alerts_sync(enabled)
                            {
                                log::error!("Failed to save refresh alerts: {err}");
                            }
                            Task::none()
                        }
                        settings::Message::SelectRefreshAlertDays(settings::AlertDays(days)) => {
                            if let Some(store) = &mut self.account_store
                                && let Err(err) = store.set_refresh_alert_days_sync(days)
                            {
                                log::error!("Failed to save the refresh alert window: {err}");
                            }
                            Task::none()
                        }
                        settings::Message::TailscaleChanged(change) => {
                            let Some(store) = &mut self.account_store else {
                                return Task::none();
                            };
                            let previous = store.tailscale().clone();
                            match change {
                                settings::TailscaleChange::Save { settings, auth_key } => {
                                    let restart = auth_key.is_some()
                                        || previous.enabled != settings.enabled
                                        || previous.hostname != settings.hostname
                                        || previous.control_url != settings.control_url;
                                    if let Err(err) = store.set_tailscale_sync(settings.clone()) {
                                        log::error!("Failed to save Tailscale settings: {err}");
                                    }
                                    Self::apply_tailscale(&settings, auth_key, restart);
                                }
                                settings::TailscaleChange::SignOut => {
                                    let settings = plume_store::TailscaleSettings {
                                        enabled: false,
                                        ..previous
                                    };
                                    if let Err(err) = store.set_tailscale_sync(settings) {
                                        log::error!("Failed to save Tailscale settings: {err}");
                                    }
                                    plume_utils::stop_tailnet(Some(
                                        defaults::get_data_path().join("tailscale_keys.json"),
                                    ));
                                }
                            }
                            Task::none()
                        }
                        settings::Message::JitChanged(settings) => {
                            if let Some(store) = &mut self.account_store
                                && let Err(err) = store.set_jit_server_sync(settings.clone())
                            {
                                log::error!("Failed to save JIT server settings: {err}");
                            }
                            if let Some(devices) = REFRESH_DAEMON_DEVICES.get() {
                                crate::jit_server::apply(&settings, devices.clone());
                            }
                            Task::none()
                        }
                        settings::Message::ToggleKeyring(enabled) => {
                            if let Some(store) = &mut self.account_store
                                && let Err(err) = store.set_use_keyring_sync(enabled)
                            {
                                log::error!("Failed to update keyring setting: {err}");
                            }
                            Task::none()
                        }
                        settings::Message::Pairing(
                            settings::PairingMessage::ToggleRemotePairing(enabled),
                        ) => {
                            self.set_remote_pairing(enabled);
                            self.utilities.rppairing_enabled = enabled;
                            Task::none()
                        }
                        settings::Message::Sources(ref sources_msg)
                            if sources_msg.changes_sources() =>
                        {
                            let task = screen
                                .update(msg, self.account_store.as_ref())
                                .map(Message::SettingsScreen);
                            Task::batch([
                                task,
                                Self::quick_install_task(quick_install::Message::Load),
                            ])
                        }
                        settings::Message::SelectLocale(choice) => {
                            self.selected_locale = choice.clone();
                            let effective = choice
                                .clone()
                                .or_else(|| current_locale::current_locale().ok())
                                .unwrap_or_else(|| "en".to_string());
                            rust_i18n::set_locale(&effective);
                            if let Some(store) = &mut self.account_store
                                && let Err(err) = store.set_locale_sync(choice)
                            {
                                log::error!("Failed to persist locale: {err}");
                            }
                            Task::none()
                        }
                        _ => screen
                            .update(msg, self.account_store.as_ref())
                            .map(Message::SettingsScreen),
                    }
                } else {
                    Task::none()
                }
            }
            Message::InstallerScreen(msg) => {
                if let ImpactorScreen::Installer(ref mut screen) = self.current_screen {
                    match msg {
                        package::Message::Back => Task::done(Message::PreviousScreen),
                        package::Message::RequestInstallation => {
                            if screen.selected_package.is_none() {
                                return Task::none();
                            }

                            use plume_utils::SignerMode;
                            if matches!(screen.options.mode, SignerMode::Pem)
                                && self
                                    .account_store
                                    .as_ref()
                                    .and_then(|s| s.selected_account())
                                    .is_none()
                            {
                                self.pending_installation = true;

                                let (login_window, task) = login_window::LoginWindow::new();
                                let id = login_window.window_id().unwrap();
                                self.login_windows.insert(id, login_window);
                                return task.map(move |msg| Message::LoginWindow(id, msg));
                            }

                            self.start_installation_task()
                        }
                        _ => screen.update(msg).map(Message::InstallerScreen),
                    }
                } else {
                    Task::none()
                }
            }
            Message::ProgressScreen(msg) => {
                if let ImpactorScreen::Progress(ref mut screen) = self.current_screen {
                    match msg {
                        progress::Message::Back => Task::done(Message::PreviousScreen),
                        progress::Message::InstallationFinished => {
                            Task::done(Message::UpdateTrayMenu)
                        }
                        _ => screen.update(msg).map(Message::ProgressScreen),
                    }
                } else {
                    Task::none()
                }
            }
            Message::RefreshAppNow { udid, app_path } => {
                if let Some(daemon_devices) = REFRESH_DAEMON_DEVICES.get() {
                    let daemon_devices = daemon_devices.clone();
                    let store_opt = self.account_store.clone();

                    std::thread::spawn(move || {
                        let rt = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .unwrap();

                        rt.block_on(async move {
                            if let Some(store) = store_opt
                                && let Some(refresh_device) = store.get_refresh_device(&udid)
                                && let Some(app) = refresh_device
                                    .apps
                                    .iter()
                                    .find(|a| a.path.to_string_lossy() == app_path)
                            {
                                let start = std::time::Instant::now();
                                let timeout = std::time::Duration::from_secs(60);

                                log::info!(
                                    "Attempting to refresh app at {:?} on device {}, waiting...",
                                    app.path,
                                    udid
                                );

                                let device_opt = loop {
                                    if start.elapsed() > timeout {
                                        log::error!("Timeout waiting for device {}", udid);
                                        break None;
                                    }

                                    if let Ok(devices) = daemon_devices.lock()
                                        && let Some(dev) = devices.get(&udid)
                                    {
                                        break Some(dev.clone());
                                    }
                                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                                };

                                if let Some(device) = device_opt {
                                    let daemon = crate::refresh::RefreshDaemon::new();
                                    if let Err(e) = daemon
                                        .refresh_app(&store, refresh_device, app, &device)
                                        .await
                                    {
                                        log::error!(
                                            "Failed to refresh app at {:?} on device {}: {}",
                                            app.path,
                                            udid,
                                            e
                                        );
                                        notify_rust::Notification::new()
                                            .summary(crate::APP_NAME)
                                            .body(&format!(
                                                "Failed to refresh {} for {}: {}",
                                                app.name.as_deref().unwrap_or("???"),
                                                refresh_device.name,
                                                e
                                            ))
                                            .show()
                                            .ok();
                                    }
                                }
                            }
                        });
                    });
                }
                Task::done(Message::UpdateTrayMenu)
            }
            Message::ForgetApp { udid, app_path } => {
                if let Some(store) = &mut self.account_store
                    && let Some(mut refresh_device) = store.get_refresh_device(&udid).cloned()
                {
                    if let Some(app) = refresh_device
                        .apps
                        .iter()
                        .find(|a| a.path.to_string_lossy() == app_path)
                    {
                        let app_path_buf = app.path.clone();
                        std::thread::spawn(move || {
                            if app_path_buf.exists() {
                                if let Err(e) = std::fs::remove_dir_all(&app_path_buf) {
                                    log::error!(
                                        "Failed to delete app at {:?}: {}",
                                        app_path_buf,
                                        e
                                    );
                                } else {
                                    log::info!("Deleted app at {:?}", app_path_buf);
                                }
                            }
                        });
                    }

                    refresh_device
                        .apps
                        .retain(|a| a.path.to_string_lossy() != app_path);

                    if refresh_device.apps.is_empty() {
                        let _ = store.remove_refresh_device_sync(&udid);
                    } else {
                        let _ = store.add_or_update_refresh_device_sync(refresh_device);
                    }

                    self.account_store = Some(Self::init_account_store_sync());
                }
                Task::done(Message::UpdateTrayMenu)
            }
            Message::UpdateTrayMenu => {
                self.account_store = Some(Self::init_account_store_sync());

                if let Some(store) = &self.account_store {
                    match &mut self.tray {
                        Some(existing_tray) => {
                            existing_tray.update_refresh_apps(store);
                        }
                        None => {
                            let mut new_tray = ImpactorTray::new();
                            new_tray.update_refresh_apps(store);
                            self.tray = Some(new_tray);
                        }
                    }
                }
                Task::none()
            }
            Message::StartInstallation => Task::none(),
            Message::SystemThemeChanged(mode) => {
                self.system_theme = mode;
                Task::none()
            }
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let device_subscription = subscriptions::device_listener();

        let tray_subscription = subscriptions::tray_subscription();

        let hover_subscription =
            if matches!(self.current_screen, ImpactorScreen::Main(_)) && self.settings.is_none() {
                subscriptions::file_hover_subscription()
            } else {
                Subscription::none()
            };

        let progress_subscription =
            if let ImpactorScreen::Progress(ref progress) = self.current_screen {
                subscriptions::installation_progress_listener(progress.progress_rx.clone()).map(
                    |(status, progress_val)| {
                        Message::ProgressScreen(progress::Message::InstallationProgress(
                            status,
                            progress_val,
                        ))
                    },
                )
            } else {
                Subscription::none()
            };

        let tab_subscription = event::listen_with(|event, status, _window| match (event, status) {
            (
                Event::Keyboard(keyboard::Event::KeyPressed {
                    key: Key::Named(key::Named::Tab),
                    modifiers,
                    ..
                }),
                Status::Ignored,
            ) => Some(Message::TabPressed {
                shift: modifiers.shift(),
            }),
            (
                Event::Keyboard(keyboard::Event::KeyPressed {
                    key: Key::Named(key::Named::Escape),
                    ..
                }),
                _,
            ) => Some(Message::CloseSettings),
            _ => None,
        });

        let logs_subscription = match self.settings.as_deref() {
            Some(screen) if screen.tab == settings::SettingsTab::Logs => {
                subscriptions::logs_tick_subscription()
            }
            Some(screen)
                if matches!(
                    screen.tab,
                    settings::SettingsTab::Tailscale | settings::SettingsTab::Jit
                ) =>
            {
                subscriptions::status_tick_subscription()
            }
            _ => Subscription::none(),
        };

        let utilities_subscription = self.utilities.subscription().map(Message::UtilitiesScreen);

        let tray_menu_refresh_subscription = subscriptions::tray_menu_refresh_subscription();
        let certificate_reset_subscription = subscriptions::certificate_reset_subscription();
        let relaunch_subscription = subscriptions::relaunch_subscription();

        let close_subscription = iced::event::listen_with(|event, _status, id| {
            if let iced::Event::Window(window::Event::CloseRequested) = event {
                return Some(Message::WindowCloseRequested(id));
            }
            None
        });

        Subscription::batch(vec![
            device_subscription,
            tray_subscription,
            hover_subscription,
            progress_subscription,
            tray_menu_refresh_subscription,
            certificate_reset_subscription,
            relaunch_subscription,
            close_subscription,
            tab_subscription,
            logs_subscription,
            utilities_subscription,
            iced::system::theme_changes().map(Message::SystemThemeChanged),
        ])
    }

    pub fn view(&self, window_id: window::Id) -> Element<'_, Message> {
        if let Some(login_window) = self.login_windows.get(&window_id) {
            return login_window
                .view()
                .map(move |msg| Message::LoginWindow(window_id, msg));
        }

        let has_device = self
            .device_lock
            .as_ref()
            .map_or(self.selected_device.is_some(), |lock| {
                lock.can_install(self.selected_device.as_ref())
            });
        let screen_content = self.view_current_screen(has_device);
        let base: Element<'_, Message> = container(
            row![
                self.view_sidebar(),
                container(screen_content).width(Fill).height(Fill)
            ]
            .height(Fill)
            .spacing(appearance::THEME_PADDING),
        )
        .padding(appearance::THEME_PADDING)
        .width(Fill)
        .height(Fill)
        .into();

        let mut layers = vec![base];
        if let Some(screen) = &self.settings {
            layers.push(self.view_settings_popup(screen));
        }
        if self.certificate_reset_queue.front().is_some() {
            layers.push(self.view_certificate_reset_prompt());
        }
        Stack::with_children(layers).into()
    }

    fn view_current_screen(&self, has_device: bool) -> Element<'_, Message> {
        match &self.current_screen {
            ImpactorScreen::Main(screen) => screen
                .view(
                    self.quick_install
                        .view(has_device)
                        .map(general::Message::QuickInstall),
                )
                .map(Message::MainScreen),
            ImpactorScreen::Utilities => self.utilities.view().map(Message::UtilitiesScreen),
            ImpactorScreen::Installer(screen) => {
                screen.view(has_device).map(Message::InstallerScreen)
            }
            ImpactorScreen::Progress(screen) => screen.view().map(Message::ProgressScreen),
        }
    }

    fn view_device_hero(&self) -> Element<'_, Message> {
        let locking = self.lock_enabled();
        let device_names: Vec<String> = self.devices.iter().map(|d| d.to_string()).collect();
        let shown_device = if locking {
            self.device_candidate.as_ref()
        } else {
            self.selected_device.as_ref()
        };
        let selected_device_name = shown_device.map(|d| d.to_string());
        let waiting = self
            .device_lock
            .as_ref()
            .filter(|_| locking && self.selected_device.is_none())
            .map(|lock| format!("Waiting for {}", lock.name));
        let placeholder_str = selected_device_name
            .as_deref()
            .or(waiting.as_deref())
            .map(str::to_owned)
            .unwrap_or_else(|| rust_i18n::t!("utilities_no_device_connected").to_string());

        let mut picker = column![
            pick_list(
                device_names,
                selected_device_name.clone(),
                Message::ComboBoxSelected
            )
            .style(appearance::s_pick_list)
            .placeholder(placeholder_str)
            .text_size(appearance::THEME_FONT_SIZE + 2.0)
            .padding(appearance::THEME_PADDING)
            .width(Fill),
        ]
        .spacing(appearance::THEME_PADDING);
        if locking {
            // Select is highlighted while the picker shows a device other than the locked one.
            let pending = self.device_candidate.as_ref().is_some_and(|candidate| {
                self.device_lock
                    .as_ref()
                    .is_none_or(|lock| !lock.accepts(candidate))
            });
            picker = picker.push(
                button(text("Select").align_x(Alignment::Center))
                    .on_press_maybe(pending.then_some(Message::SelectDevice))
                    .style(if pending {
                        appearance::p_button
                    } else {
                        appearance::s_button
                    })
                    .width(Fill),
            );
        }
        picker.into()
    }

    fn view_sidebar(&self) -> Element<'_, Message> {
        // The installer and progress screens have their own way back.
        let can_navigate = matches!(
            self.current_screen,
            ImpactorScreen::Main(_) | ImpactorScreen::Utilities
        );
        let section = |label: std::borrow::Cow<'static, str>, active: bool, message: Message| {
            button(text(label).align_x(Alignment::Start))
                .on_press_maybe(can_navigate.then_some(message))
                .style(if active {
                    appearance::p_button
                } else {
                    appearance::s_button
                })
                .width(Fill)
        };

        let mut sidebar = column![
            self.view_device_hero(),
            section(
                rust_i18n::t!("install"),
                !matches!(self.current_screen, ImpactorScreen::Utilities),
                Message::NavigateToScreen(ImpactorScreenType::Main),
            ),
        ]
        .spacing(appearance::THEME_PADDING)
        .width(SIDEBAR_WIDTH)
        .height(Fill);
        for tab in utilties::UtilitiesTab::ALL {
            let active = matches!(self.current_screen, ImpactorScreen::Utilities)
                && self.utilities.tab() == tab;
            sidebar = sidebar.push(section(tab.label(), active, Message::SelectDeviceTab(tab)));
        }

        sidebar
            .push(container(text("")).height(Fill))
            .push(
                button(appearance::icon_text(
                    appearance::GEAR,
                    rust_i18n::t!("settings_title"),
                    None,
                ))
                .on_press(Message::OpenSettings)
                .style(appearance::s_button)
                .width(Fill),
            )
            .into()
    }

    fn view_settings_popup<'a>(
        &'a self,
        screen: &'a settings::SettingsScreen,
    ) -> Element<'a, Message> {
        let header = row![
            text(rust_i18n::t!("settings_title"))
                .size(appearance::THEME_FONT_SIZE + 2.0)
                .width(Fill),
            button(text(rust_i18n::t!("close")))
                .on_press(Message::CloseSettings)
                .style(appearance::s_button),
        ]
        .align_y(Alignment::Center)
        .spacing(appearance::THEME_PADDING);

        let body = screen
            .view(
                &self.account_store,
                &self.selected_locale,
                self.selected_device.as_ref(),
                &self.devices,
            )
            .map(Message::SettingsScreen);

        let dialog = container(column![header, body].spacing(appearance::THEME_PADDING))
            .padding(appearance::THEME_PADDING)
            .width(SETTINGS_POPUP_WIDTH)
            .height(SETTINGS_POPUP_HEIGHT)
            .style(|theme: &iced::Theme| container::Style {
                background: Some(iced::Background::Color(theme.palette().background)),
                border: iced::Border {
                    width: 1.0,
                    color: theme.palette().text.scale_alpha(0.2),
                    radius: appearance::THEME_CORNER_RADIUS.into(),
                },
                ..Default::default()
            });

        opaque(
            mouse_area(container(opaque(dialog)).center(Fill).style(popup_backdrop))
                .on_press(Message::CloseSettings),
        )
    }

    fn view_certificate_reset_prompt(&self) -> Element<'_, Message> {
        let Some(request) = self.certificate_reset_queue.front() else {
            return container(text("")).into();
        };

        let actions = row![
            button(text("Cancel"))
                .on_press(Message::CancelCertificateReset)
                .style(appearance::s_button),
            button(text("Continue"))
                .on_press(Message::ConfirmCertificateReset)
                .style(appearance::p_button),
        ]
        .spacing(appearance::THEME_PADDING);

        let dialog = container(
            column![
                text("Certificate reset required").size(appearance::THEME_FONT_SIZE + 2.0),
                text(&request.message),
                actions,
            ]
            .spacing(appearance::THEME_PADDING),
        )
        .padding(appearance::THEME_PADDING * 2.0)
        .max_width(420.0)
        .style(|theme: &iced::Theme| container::Style {
            background: Some(iced::Background::Color(theme.palette().background)),
            border: iced::Border {
                width: 1.0,
                color: theme.palette().warning,
                radius: appearance::THEME_CORNER_RADIUS.into(),
            },
            ..Default::default()
        });

        container(dialog)
            .width(Fill)
            .height(Fill)
            .center(Fill)
            .style(popup_backdrop)
            .into()
    }

    fn navigate_to_screen(&mut self, screen_type: ImpactorScreenType) {
        match screen_type {
            ImpactorScreenType::Main => {
                if let ImpactorScreen::Installer(installer) = &self.current_screen {
                    Self::cleanup_package_stage(installer);
                }
                if let Some(installer) = self.installer_screen.take() {
                    Self::cleanup_package_stage(&installer);
                }

                self.current_screen = ImpactorScreen::Main(general::GeneralScreen::new());
            }
            ImpactorScreenType::Progress => {
                self.current_screen = ImpactorScreen::Progress(progress::ProgressScreen::new());
            }
            _ => {}
        }
    }

    fn cleanup_package_stage(installer: &package::PackageScreen) {
        if std::env::var("PLUME_DELETE_AFTER_FINISHED").is_ok() {
            return;
        }
        if let Some(package) = installer.selected_package.clone() {
            package.remove_package_stage();
        }
    }

    /// Installs an app from a source, asking the user to sign in first if needed.
    fn request_source_install(
        &mut self,
        app: crate::sources::SourceApp,
        version: crate::sources::AppVersion,
    ) -> Task<Message> {
        if self
            .account_store
            .as_ref()
            .is_none_or(|store| store.selected_account().is_none() && store.asc_key().is_none())
        {
            self.pending_source_install = Some((app, version));
            let (login_window, task) = login_window::LoginWindow::new();
            let id = login_window.window_id().unwrap();
            self.login_windows.insert(id, login_window);
            return task.map(move |msg| Message::LoginWindow(id, msg));
        }

        self.start_source_install(app, version)
    }

    fn start_source_install(
        &mut self,
        app: crate::sources::SourceApp,
        version: crate::sources::AppVersion,
    ) -> Task<Message> {
        if self
            .device_lock
            .as_ref()
            .is_some_and(|lock| !lock.can_install(self.selected_device.as_ref()))
        {
            return Task::none();
        }

        let device = self.selected_device.clone();
        let account = self
            .account_store
            .as_ref()
            .and_then(|s| s.selected_account().cloned());
        let mut store = self.account_store.clone();

        let (tx, rx) = std::sync::mpsc::channel();
        let mut progress_screen = progress::ProgressScreen::new();
        progress_screen.start_installation(std::sync::Arc::new(std::sync::Mutex::new(rx)));
        self.current_screen = ImpactorScreen::Progress(progress_screen);

        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async move {
                let result = subscriptions::run_source_install(
                    &app,
                    &version,
                    device.as_ref(),
                    account.as_ref(),
                    store.as_mut(),
                    &tx,
                )
                .await;
                match result {
                    Ok(()) => {
                        log::info!("Installed {} {}", app.name, version.version);
                        let _ = tx.send(("Installation complete!".to_string(), 100));
                    }
                    Err(e) => {
                        log::error!("Failed to install {} {}: {e}", app.name, version.version);
                        let _ = tx.send((format!("Error: {}", e), -1));
                    }
                }
            });
        });

        Task::none()
    }

    fn start_installation_task(&mut self) -> Task<Message> {
        if self
            .device_lock
            .as_ref()
            .is_some_and(|lock| !lock.can_install(self.selected_device.as_ref()))
        {
            let exporting = match &self.current_screen {
                ImpactorScreen::Installer(screen) => {
                    screen.options.install_mode == plume_utils::SignerInstallMode::Export
                }
                _ => false,
            };
            if !exporting {
                return Task::none();
            }
        }

        if let ImpactorScreen::Installer(installer) = &self.current_screen {
            let Some(package) = installer.selected_package.clone() else {
                return Task::none();
            };

            let device = self.selected_device.clone();
            let options = installer.options.clone();
            let account = self
                .account_store
                .as_ref()
                .and_then(|s| s.selected_account().cloned());
            let mut store = self.account_store.clone();

            let (tx, rx) = std::sync::mpsc::channel();
            let progress_rx = std::sync::Arc::new(std::sync::Mutex::new(rx));

            let mut progress_screen = progress::ProgressScreen::new();
            progress_screen.start_installation(progress_rx.clone());

            let previous = std::mem::replace(
                &mut self.current_screen,
                ImpactorScreen::Progress(progress_screen),
            );
            if let ImpactorScreen::Installer(installer) = previous
                && let Some(stale) = self.installer_screen.replace(installer)
            {
                Self::cleanup_package_stage(&stale);
            }

            std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                let tx_error = tx.clone();
                rt.block_on(async move {
                    match subscriptions::run_installation(
                        &package,
                        device.as_ref(),
                        &options,
                        account.as_ref(),
                        store.as_mut(),
                        &tx,
                    )
                    .await
                    {
                        Ok(_) => {
                            let _ = tx.send(("Installation complete!".to_string(), 100));
                        }
                        Err(e) => {
                            let _ = tx_error.send((format!("Error: {}", e), -1));
                        }
                    }
                });
            });

            Task::none()
        } else {
            Task::none()
        }
    }
}

fn popup_backdrop(_theme: &iced::Theme) -> container::Style {
    container::Style {
        background: Some(iced::Background::Color(iced::Color {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.45,
        })),
        ..Default::default()
    }
}
