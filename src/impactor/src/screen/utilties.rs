use iced::widget::{
    button, checkbox, column, container, image, row, rule, scrollable, text, text_input,
};
use iced::{Center, Color, Element, Fill, Subscription, Task};
use rust_i18n::t;

use crate::appearance;
use crate::defaults::get_data_path;
use plume_utils::{Device, SignerAppReal};
use std::collections::{HashMap, VecDeque};

// Older syslog lines are dropped past this; saving writes what is buffered.
const SYSLOG_BUFFER: usize = 20_000;
// Drawing every buffered line as a widget gets slow.
const SYSLOG_SHOWN: usize = 500;

#[derive(Debug, Clone)]
struct StatusMessage {
    text: String,
    is_error: bool,
}

impl StatusMessage {
    fn success(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: false,
        }
    }

    fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: true,
        }
    }

    fn color(&self) -> Color {
        if self.is_error {
            Color::from_rgb(0.9, 0.2, 0.2)
        } else {
            Color::from_rgb(0.2, 0.8, 0.4)
        }
    }
}

#[allow(unused)]
#[derive(Debug, Clone)]
pub enum Message {
    RefreshApps(bool),
    AppsLoaded(Result<Vec<SignerAppReal>, String>),
    IconsLoaded(HashMap<String, Vec<u8>>),
    InstallPairingFile(SignerAppReal),
    Trust,
    PairResult(Result<(), String>),
    InstallPairingResult(String, Result<(), String>),
    ToggleRPPairing(bool),
    SelectTab(UtilitiesTab),
    SyslogStart,
    SyslogStop,
    SyslogLines(Vec<String>),
    SyslogEnded(Result<(), String>),
    SyslogTogglePause,
    SyslogClear,
    SyslogFilterChanged(String),
    SyslogCopy,
    SyslogSave,
    SyslogSaved(Result<bool, String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UtilitiesTab {
    #[default]
    Apps,
    Syslog,
}

#[derive(Debug, Clone, Default)]
struct SyslogState {
    running: bool,
    /// Bumped on every start so the subscription reconnects.
    session: u64,
    paused: bool,
    lines: VecDeque<String>,
    /// Lines that arrived while paused, shown on resume.
    held: Vec<String>,
    filter: String,
    status: Option<StatusMessage>,
}

impl SyslogState {
    fn push(&mut self, lines: Vec<String>) {
        if self.paused {
            self.held.extend(lines);
            let excess = self.held.len().saturating_sub(SYSLOG_BUFFER);
            self.held.drain(..excess);
            return;
        }
        self.lines.extend(lines);
        let excess = self.lines.len().saturating_sub(SYSLOG_BUFFER);
        self.lines.drain(..excess);
    }

    fn matching(&self) -> impl DoubleEndedIterator<Item = &String> {
        let filter = self.filter.trim().to_lowercase();
        self.lines
            .iter()
            .filter(move |line| filter.is_empty() || line.to_lowercase().contains(&filter))
    }
}

#[derive(Debug, Clone)]
pub struct UtilitiesScreen {
    device: Option<Device>,
    installed_apps: Vec<SignerAppReal>,
    status_message: Option<StatusMessage>,
    app_statuses: HashMap<String, StatusMessage>,
    app_icons: HashMap<String, image::Handle>,
    loading: bool,
    trust_loading: bool,
    pub rppairing_enabled: bool,
    tab: UtilitiesTab,
    syslog: SyslogState,
}

impl UtilitiesScreen {
    pub fn new(device: Option<Device>, rppairing_enabled: bool) -> Self {
        let mut screen = Self {
            device,
            installed_apps: Vec::new(),
            status_message: None,
            app_statuses: HashMap::new(),
            app_icons: HashMap::new(),
            loading: false,
            trust_loading: false,
            rppairing_enabled,
            tab: UtilitiesTab::default(),
            syslog: SyslogState::default(),
        };

        if screen.device.as_ref().map(|d| d.is_mac).unwrap_or(false) {
            screen.status_message = Some(StatusMessage::error(t!(
                "utilities_mac_devices_not_supported"
            )));
        }

        screen
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::RefreshApps(_) => {
                self.loading = true;
                self.status_message = None;
                self.app_statuses.clear();
                if let Some(device) = &self.device {
                    if device.is_mac || !device.is_available() {
                        self.loading = false;
                        return Task::none();
                    }

                    let device = device.clone();
                    let (tx, rx) = std::sync::mpsc::sync_channel(1);

                    std::thread::spawn(move || {
                        let rt = tokio::runtime::Runtime::new().unwrap();
                        let result = rt.block_on(async move {
                            device
                                .installed_apps()
                                .await
                                .map_err(|e| format!("Failed to load apps: {}", e))
                        });
                        let _ = tx.send(result);
                    });

                    Task::perform(
                        async move {
                            std::thread::spawn(move || {
                                rx.recv()
                                    .unwrap_or_else(|_| Err("Failed to receive result".to_string()))
                            })
                            .join()
                            .unwrap()
                        },
                        Message::AppsLoaded,
                    )
                } else {
                    Task::done(Message::AppsLoaded(Err("No device connected".to_string())))
                }
            }
            Message::AppsLoaded(result) => {
                self.loading = false;
                match result {
                    Ok(apps) => {
                        self.installed_apps = apps;
                        self.status_message = None;
                        return self.load_icons();
                    }
                    Err(e) => {
                        self.status_message = Some(StatusMessage::error(e));
                        self.installed_apps.clear();
                    }
                }
                Task::none()
            }
            Message::IconsLoaded(icons) => {
                self.app_icons.extend(icons.into_iter().filter_map(|(bundle_id, png)| {
                    super::package::icon_handle_from_bytes(&png).map(|handle| (bundle_id, handle))
                }));
                Task::none()
            }
            Message::InstallPairingFile(app) => {
                if let Some(device) = &self.device {
                    let device = device.clone();
                    let bundle_id = app.bundle_id.clone().unwrap_or_default();
                    let signer_app = app.app;
                    let app_key = Self::app_key(&app);
                    let (tx, rx) = std::sync::mpsc::sync_channel(1);

                    let rppairing_enabled = self.rppairing_enabled;

                    std::thread::spawn(move || {
                        let rt = tokio::runtime::Runtime::new().unwrap();
                        let result = rt.block_on(async move {
                            device
                                .install_pairing_files(
                                    &bundle_id,
                                    signer_app,
                                    rppairing_enabled,
                                    get_data_path(),
                                )
                                .await
                                .map_err(|e| {
                                    log::error!("Failed to install pairing file: {e}");
                                    format!("Failed to install pairing file: {}", e)
                                })
                        });
                        let _ = tx.send(result);
                    });

                    Task::perform(
                        async move {
                            std::thread::spawn(move || {
                                rx.recv()
                                    .unwrap_or_else(|_| Err("Failed to receive result".to_string()))
                            })
                            .join()
                            .unwrap()
                        },
                        move |result| Message::InstallPairingResult(app_key, result),
                    )
                } else {
                    Task::none()
                }
            }
            Message::Trust => {
                self.trust_loading = true;
                self.status_message = None;
                if let Some(device) = &self.device {
                    let device = device.clone();
                    let (tx, rx) = std::sync::mpsc::sync_channel(1);

                    std::thread::spawn(move || {
                        let rt = tokio::runtime::Runtime::new().unwrap();
                        let result = rt.block_on(async move {
                            device
                                .pair()
                                .await
                                .map_err(|e| format!("Failed to pair: {}", e))
                        });
                        let _ = tx.send(result);
                    });

                    Task::perform(
                        async move {
                            std::thread::spawn(move || {
                                rx.recv()
                                    .unwrap_or_else(|_| Err("Failed to receive result".to_string()))
                            })
                            .join()
                            .unwrap()
                        },
                        Message::PairResult,
                    )
                } else {
                    Task::none()
                }
            }
            Message::PairResult(result) => {
                self.trust_loading = false;
                match result {
                    Ok(_) => {
                        self.status_message =
                            Some(StatusMessage::success(t!("utilities_paired_success")));
                    }
                    Err(e) => {
                        self.status_message = Some(StatusMessage::error(e));
                    }
                }
                Task::none()
            }
            Message::InstallPairingResult(app_key, result) => {
                let status = match result {
                    Ok(_) => StatusMessage::success(t!("utilities_device_paired_success")),
                    Err(e) => StatusMessage::error(e),
                };
                self.app_statuses.insert(app_key, status);
                Task::none()
            }
            Message::ToggleRPPairing(enabled) => {
                self.rppairing_enabled = enabled;
                Task::none()
            }
            Message::SelectTab(tab) => {
                self.tab = tab;
                Task::none()
            }
            Message::SyslogStart => {
                if self
                    .device
                    .as_ref()
                    .is_some_and(|d| !d.is_mac && d.is_available())
                {
                    self.syslog.running = true;
                    self.syslog.session += 1;
                    self.syslog.status = None;
                } else {
                    self.syslog.status =
                        Some(StatusMessage::error(t!("utilities_no_device_connected")));
                }
                Task::none()
            }
            Message::SyslogStop => {
                self.syslog.running = false;
                Task::none()
            }
            Message::SyslogLines(lines) => {
                if self.syslog.running {
                    self.syslog.push(lines);
                }
                Task::none()
            }
            Message::SyslogEnded(result) => {
                self.syslog.running = false;
                if let Err(e) = result {
                    log::warn!("{e}");
                    self.syslog.status = Some(StatusMessage::error(e));
                }
                Task::none()
            }
            Message::SyslogTogglePause => {
                self.syslog.paused = !self.syslog.paused;
                if !self.syslog.paused {
                    let held = std::mem::take(&mut self.syslog.held);
                    self.syslog.push(held);
                }
                Task::none()
            }
            Message::SyslogClear => {
                self.syslog.lines.clear();
                self.syslog.held.clear();
                Task::none()
            }
            Message::SyslogFilterChanged(filter) => {
                self.syslog.filter = filter;
                Task::none()
            }
            Message::SyslogCopy => {
                let contents = self
                    .syslog
                    .matching()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n");
                iced::clipboard::write(contents)
            }
            Message::SyslogSave => {
                let mut contents = self
                    .syslog
                    .lines
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n");
                contents.push('\n');
                let file_name = format!(
                    "syslog-{}-{}.log",
                    self.device
                        .as_ref()
                        .map(|d| d.name.as_str())
                        .unwrap_or("device"),
                    chrono::Local::now().format("%Y%m%d-%H%M%S")
                );
                Task::perform(
                    async move {
                        let Some(handle) = rfd::AsyncFileDialog::new()
                            .set_file_name(file_name)
                            .add_filter("Log", &["log", "txt"])
                            .save_file()
                            .await
                        else {
                            return Ok(false);
                        };
                        std::fs::write(handle.path(), contents)
                            .map(|_| true)
                            .map_err(|e| format!("Failed to save syslog: {e}"))
                    },
                    Message::SyslogSaved,
                )
            }
            Message::SyslogSaved(result) => {
                match result {
                    Ok(true) => {
                        self.syslog.status =
                            Some(StatusMessage::success(t!("utilities_syslog_saved")))
                    }
                    Ok(false) => {}
                    Err(e) => self.syslog.status = Some(StatusMessage::error(e)),
                }
                Task::none()
            }
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        match &self.device {
            Some(device) if self.syslog.running => {
                crate::subscriptions::syslog_subscription(device.clone(), self.syslog.session)
            }
            _ => Subscription::none(),
        }
    }

    pub fn view(&self) -> Element<'_, Message> {
        let tab_button = |tab: UtilitiesTab, label: String| {
            button(text(label).align_x(Center))
                .on_press(Message::SelectTab(tab))
                .style(if self.tab == tab {
                    appearance::p_button
                } else {
                    appearance::s_button
                })
                .width(Fill)
        };
        let tabs = row![
            tab_button(UtilitiesTab::Apps, t!("utilities_tab_apps").to_string()),
            tab_button(UtilitiesTab::Syslog, t!("utilities_tab_syslog").to_string()),
        ]
        .spacing(appearance::THEME_PADDING);

        let body = match self.tab {
            UtilitiesTab::Apps => self.view_apps(),
            UtilitiesTab::Syslog => self.view_syslog(),
        };

        column![tabs, body]
            .spacing(appearance::THEME_PADDING)
            .height(Fill)
            .into()
    }

    fn view_syslog(&self) -> Element<'_, Message> {
        let small = appearance::THEME_FONT_SIZE - 1.0;
        let usable = self.device.as_ref().is_some_and(|d| !d.is_mac);

        let start_stop = if self.syslog.running {
            button(text(t!("utilities_syslog_stop")).align_x(Center))
                .on_press(Message::SyslogStop)
                .style(appearance::s_button)
        } else {
            button(text(t!("utilities_syslog_start")).align_x(Center))
                .on_press_maybe(usable.then_some(Message::SyslogStart))
                .style(appearance::p_button)
        };
        let pause_label = if self.syslog.paused {
            t!("utilities_syslog_resume")
        } else {
            t!("utilities_syslog_pause")
        };

        let toolbar = row![
            start_stop,
            button(text(pause_label).align_x(Center))
                .on_press(Message::SyslogTogglePause)
                .style(appearance::s_button),
            text_input(&t!("utilities_syslog_filter"), &self.syslog.filter)
                .on_input(Message::SyslogFilterChanged)
                .width(Fill),
            button(text(t!("logs_copy")))
                .on_press(Message::SyslogCopy)
                .style(appearance::s_button),
            button(text(t!("utilities_syslog_save")))
                .on_press_maybe((!self.syslog.lines.is_empty()).then_some(Message::SyslogSave))
                .style(appearance::s_button),
            button(text(t!("logs_clear")))
                .on_press(Message::SyslogClear)
                .style(appearance::s_button),
        ]
        .spacing(appearance::THEME_PADDING)
        .align_y(Center);

        let mut content = column![toolbar].spacing(appearance::THEME_PADDING);

        let status = if let Some(ref status) = self.syslog.status {
            Some(text(&status.text).size(small).color(status.color()))
        } else if !usable {
            Some(
                text(t!("utilities_no_device_connected"))
                    .size(small)
                    .color(Color::from_rgb(0.7, 0.7, 0.7)),
            )
        } else if self.syslog.paused && !self.syslog.held.is_empty() {
            Some(text(t!("utilities_syslog_held", count = self.syslog.held.len())).size(small))
        } else {
            None
        };
        if let Some(status) = status {
            content = content.push(status);
        }

        let mut shown: Vec<&String> = self.syslog.matching().rev().take(SYSLOG_SHOWN).collect();
        shown.reverse();
        let lines = if shown.is_empty() {
            let placeholder = if self.syslog.running {
                t!("utilities_syslog_waiting")
            } else {
                t!("utilities_syslog_empty")
            };
            column![text(placeholder).size(small)]
        } else {
            shown.into_iter().fold(column![].spacing(1), |col, line| {
                let mut line_text = text(line.as_str()).size(small).font(appearance::p_font());
                if line.contains("<Error>") || line.contains("<Fault>") {
                    line_text = line_text.color(Color::from_rgb(0.9, 0.35, 0.35));
                } else if line.contains("<Warning>") {
                    line_text = line_text.color(Color::from_rgb(0.95, 0.75, 0.4));
                }
                col.push(line_text)
            })
        };

        content
            .push(scrollable(lines).anchor_bottom().height(Fill).width(Fill))
            .height(Fill)
            .into()
    }

    fn view_apps(&self) -> Element<'_, Message> {
        let mut content = column![].spacing(appearance::THEME_PADDING);

        if let Some(ref device) = self.device {
            content = content.push(
                column![
                    text(format!("Name: {}", device.name)),
                    text(format!("UDID: {}", device.udid)),
                ]
                .spacing(4),
            );
        } else {
            content = content.push(
                text(t!("utilities_no_device_connected")).color(Color::from_rgb(0.7, 0.7, 0.7)),
            );
        }

        if let Some(ref status) = self.status_message {
            content = content.push(text(&status.text).size(14).color(status.color()));
        }

        if self.device.is_some() && !self.device.as_ref().unwrap().is_mac {
            let refresh_button_text = if self.loading {
                t!("utilities_loading")
            } else {
                t!("utilities_refresh_installed_apps")
            };

            let trust_button_text = if self.trust_loading {
                t!("utilities_pairing")
            } else {
                t!("utilities_trust_device")
            };

            content = content.push(
                row![
                    button(text(trust_button_text).align_x(Center))
                        .on_press_maybe(if self.trust_loading {
                            None
                        } else {
                            Some(Message::Trust)
                        })
                        .style(appearance::s_button)
                        .width(iced::Length::Fill),
                    button(text(refresh_button_text).align_x(Center))
                        .on_press_maybe(if self.loading {
                            None
                        } else {
                            Some(Message::RefreshApps(self.rppairing_enabled))
                        })
                        .style(appearance::s_button)
                        .width(iced::Length::Fill),
                ]
                .spacing(appearance::THEME_PADDING),
            );
        }

        let toggle = checkbox(self.rppairing_enabled)
            .label(t!("pairing_include_remote"))
            .on_toggle(Message::ToggleRPPairing);

        content = content.push(toggle);

        // Apps without a readable provisioning profile (App Store and system apps) are hidden.
        let mut listed_apps = self
            .installed_apps
            .iter()
            .filter(|app| app.expiration.is_some())
            .peekable();
        if listed_apps.peek().is_some() {
            content = content
                .push(container(rule::horizontal(1)).padding([appearance::THEME_PADDING, 0.0]));

            let mut apps_list = column![].spacing(4);

            for app in listed_apps {
                let app_key = Self::app_key(app);
                let mut header = row![].spacing(appearance::THEME_PADDING).align_y(Center);
                if let Some(icon) = app.bundle_id.as_ref().and_then(|id| self.app_icons.get(id)) {
                    header = header.push(image(icon.clone()).width(32).height(32));
                }
                header = header.push(
                    text(format!(
                        "{} ({})",
                        app.installed_name
                            .clone()
                            .unwrap_or_else(|| app.app.to_string()),
                        app.bundle_id.as_deref().unwrap_or("???")
                    ))
                    .size(14)
                    .width(iced::Length::Fill),
                );
                if app.app.supports_pairing_file_alt() {
                    header = header.push(
                        button(text(t!("utilities_install_pairing")).align_x(Center))
                            .on_press(Message::InstallPairingFile(app.clone()))
                            .style(appearance::s_button),
                    );
                }
                let mut app_row = column![header].spacing(4);

                if let Some(date) = app.expiration {
                    let expired = std::time::SystemTime::from(date) <= std::time::SystemTime::now();
                    app_row = app_row.push(
                        text(format!(
                            "{}: {}",
                            if expired { "Expired" } else { "Expires" },
                            date.to_xml_format()
                        ))
                        .size(13),
                    );
                }
                if let Some(status) = self.app_statuses.get(&app_key) {
                    app_row = app_row.push(text(&status.text).size(13).color(status.color()));
                }

                apps_list = apps_list.push(app_row);
            }

            content = content.push(apps_list);
        }

        container(scrollable(content)).into()
    }

    /// Fetches icons for the listed apps that do not have one yet.
    fn load_icons(&self) -> Task<Message> {
        let Some(device) = self.device.clone() else {
            return Task::none();
        };
        let bundle_ids: Vec<String> = self
            .installed_apps
            .iter()
            .filter(|app| app.expiration.is_some())
            .filter_map(|app| app.bundle_id.clone())
            .filter(|bundle_id| !self.app_icons.contains_key(bundle_id))
            .collect();
        if bundle_ids.is_empty() {
            return Task::none();
        }
        Task::perform(
            crate::subscriptions::run_on_tokio(async move {
                device.app_icons(bundle_ids).await.unwrap_or_else(|error| {
                    log::warn!("Could not load app icons: {error}");
                    HashMap::new()
                })
            }),
            Message::IconsLoaded,
        )
    }

    fn app_key(app: &SignerAppReal) -> String {
        app.bundle_id.clone().unwrap_or_else(|| app.app.to_string())
    }
}
