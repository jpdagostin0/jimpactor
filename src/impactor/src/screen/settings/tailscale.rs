use iced::widget::{button, checkbox, column, container, row, scrollable, text, text_input};
use iced::{Alignment, Element, Fill, Task};
use plume_store::TailscaleSettings;
use plume_utils::TailnetStatus;
use rust_i18n::t;

use crate::appearance;

#[derive(Debug, Clone)]
pub enum Message {
    ToggleEnabled(bool),
    UpdateHostname(String),
    UpdateControlUrl(String),
    UpdateAuthKey(AuthKeyInput),
    Apply,
    SignIn(String),
    SignOut,
    UpdateNewDevice(String),
    AddDevice,
    RemoveDevice(usize),
}

#[derive(Clone, Default)]
pub struct AuthKeyInput(String);

impl std::fmt::Debug for AuthKeyInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[redacted Tailscale auth key]")
    }
}

/// A settings change for the app to save and apply.
#[derive(Debug, Clone)]
pub enum Change {
    Save {
        settings: TailscaleSettings,
        auth_key: Option<String>,
    },
    SignOut,
}

#[derive(Debug)]
pub struct TailscaleTab {
    draft: TailscaleSettings,
    control_url: String,
    auth_key: AuthKeyInput,
    new_device: String,
}

impl TailscaleTab {
    pub fn new(settings: TailscaleSettings) -> Self {
        Self {
            control_url: settings.control_url.clone().unwrap_or_default(),
            draft: settings,
            auth_key: AuthKeyInput::default(),
            new_device: String::new(),
        }
    }

    fn save(&mut self) -> Change {
        let control_url = self.control_url.trim();
        self.draft.control_url = (!control_url.is_empty()).then(|| control_url.to_owned());
        let auth_key = std::mem::take(&mut self.auth_key).0;
        Change::Save {
            settings: self.draft.clone(),
            auth_key: (!auth_key.trim().is_empty()).then(|| auth_key.trim().to_owned()),
        }
    }

    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Change>) {
        let change = match message {
            Message::ToggleEnabled(enabled) => {
                self.draft.enabled = enabled;
                Some(self.save())
            }
            Message::UpdateHostname(hostname) => {
                self.draft.hostname = hostname;
                None
            }
            Message::UpdateControlUrl(url) => {
                self.control_url = url;
                None
            }
            Message::UpdateAuthKey(key) => {
                self.auth_key = key;
                None
            }
            Message::Apply => Some(self.save()),
            Message::SignIn(url) => {
                if let Err(error) = open::that(&url) {
                    log::error!("Could not open the Tailscale sign-in page: {error}");
                }
                None
            }
            Message::SignOut => {
                self.draft.enabled = false;
                Some(Change::SignOut)
            }
            Message::UpdateNewDevice(value) => {
                self.new_device = value;
                None
            }
            Message::AddDevice => {
                let device = self.new_device.trim().to_owned();
                if device.is_empty()
                    || self
                        .draft
                        .devices
                        .iter()
                        .any(|d| d.eq_ignore_ascii_case(&device))
                {
                    return (Task::none(), None);
                }
                self.draft.devices.push(device);
                self.new_device.clear();
                Some(self.save())
            }
            Message::RemoveDevice(index) => {
                if index < self.draft.devices.len() {
                    self.draft.devices.remove(index);
                }
                Some(self.save())
            }
        };
        (Task::none(), change)
    }

    pub fn view(&self) -> Element<'_, Message> {
        let supported = plume_utils::tailnet_supported();
        let status = plume_utils::tailnet_status();
        let small = appearance::THEME_FONT_SIZE - 1.0;

        let mut content = column![
            text(t!("settings_tab_tailscale")),
            text(t!("tailscale_desc")).size(small),
        ]
        .spacing(appearance::THEME_PADDING);

        if !supported {
            content = content.push(text(t!("tailscale_unsupported")));
            return content.into();
        }

        content = content.push(
            checkbox(self.draft.enabled)
                .label(t!("tailscale_enable"))
                .on_toggle(Message::ToggleEnabled),
        );

        let status_text = match &status {
            TailnetStatus::Off => t!("tailscale_status_off").to_string(),
            TailnetStatus::Unsupported => t!("tailscale_unsupported").to_string(),
            TailnetStatus::Connecting => t!("tailscale_status_connecting").to_string(),
            TailnetStatus::LoginRequired(_) => t!("tailscale_status_login").to_string(),
            TailnetStatus::Connected { address } => {
                t!("tailscale_status_connected", address = address.to_string()).to_string()
            }
            TailnetStatus::Failed(error) => {
                t!("tailscale_status_failed", error = error.clone()).to_string()
            }
        };
        let mut status_row = row![text(status_text).width(Fill)]
            .spacing(appearance::THEME_PADDING)
            .align_y(Alignment::Center);
        if let TailnetStatus::LoginRequired(url) = &status {
            status_row = status_row.push(
                button(text(t!("tailscale_sign_in")))
                    .on_press(Message::SignIn(url.clone()))
                    .style(appearance::p_button),
            );
        }
        if !matches!(status, TailnetStatus::Off) {
            status_row = status_row.push(
                button(text(t!("tailscale_sign_out")))
                    .on_press(Message::SignOut)
                    .style(appearance::s_button),
            );
        }
        content = content.push(
            container(status_row)
                .padding(appearance::THEME_PADDING)
                .style(container::rounded_box)
                .width(Fill),
        );

        content = content.push(
            column![
                text(t!("tailscale_hostname")),
                text_input("jimpactor", &self.draft.hostname)
                    .on_input(Message::UpdateHostname)
                    .on_submit(Message::Apply)
                    .padding(6),
                text(t!("tailscale_control_url")),
                text_input(&t!("tailscale_control_url_placeholder"), &self.control_url)
                    .on_input(Message::UpdateControlUrl)
                    .on_submit(Message::Apply)
                    .padding(6),
                text(t!("tailscale_auth_key")),
                text_input(&t!("tailscale_auth_key_placeholder"), &self.auth_key.0)
                    .secure(true)
                    .on_input(|key| Message::UpdateAuthKey(AuthKeyInput(key)))
                    .on_submit(Message::Apply)
                    .padding(6),
                button(text(t!("tailscale_apply")))
                    .on_press(Message::Apply)
                    .style(appearance::s_button),
            ]
            .spacing(appearance::THEME_PADDING / 2.0),
        );

        let mut devices = column![
            text(t!("tailscale_devices")),
            text(t!("tailscale_devices_desc")).size(small),
        ]
        .spacing(appearance::THEME_PADDING / 2.0);
        for (index, device) in self.draft.devices.iter().enumerate() {
            devices = devices.push(
                row![
                    text(device).width(Fill),
                    button(text(t!("tailscale_remove_device")))
                        .on_press(Message::RemoveDevice(index))
                        .style(appearance::s_button),
                ]
                .spacing(appearance::THEME_PADDING)
                .align_y(Alignment::Center),
            );
        }
        devices = devices.push(
            row![
                text_input(&t!("tailscale_device_placeholder"), &self.new_device)
                    .on_input(Message::UpdateNewDevice)
                    .on_submit(Message::AddDevice)
                    .padding(6)
                    .width(Fill),
                button(text(t!("tailscale_add_device")))
                    .on_press_maybe(
                        (!self.new_device.trim().is_empty()).then_some(Message::AddDevice)
                    )
                    .style(appearance::s_button),
            ]
            .spacing(appearance::THEME_PADDING)
            .align_y(Alignment::Center),
        );
        content = content.push(
            container(devices)
                .padding(appearance::THEME_PADDING)
                .style(container::rounded_box)
                .width(Fill),
        );

        scrollable(content.padding([0.0, appearance::THEME_PADDING]).width(Fill))
            .height(Fill)
            .into()
    }
}
