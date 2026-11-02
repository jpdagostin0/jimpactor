use iced::widget::{button, column, container, row, text};
use iced::{Center, Element, Fill, Task};
use plume_utils::{Device, DeviceDetails};
use rust_i18n::t;

use super::StatusMessage;
use crate::appearance;
use crate::subscriptions::{confirm, run_on_tokio};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerAction {
    Restart,
    Shutdown,
}

#[derive(Debug, Clone)]
pub enum Message {
    Refresh,
    Loaded(Result<DeviceDetails, String>),
    Power(PowerAction),
    PowerConfirmed(PowerAction, bool),
    PowerDone(Result<(), String>),
}

#[derive(Debug, Clone, Default)]
pub struct DeviceTab {
    details: Option<DeviceDetails>,
    loading: bool,
    status: Option<StatusMessage>,
}

impl DeviceTab {
    pub fn update(&mut self, device: Option<&Device>, message: Message) -> Task<Message> {
        match message {
            Message::Refresh => {
                let Some(device) = device.cloned() else {
                    return Task::none();
                };
                self.loading = true;
                Task::perform(
                    run_on_tokio(async move {
                        device
                            .details()
                            .await
                            .map_err(|e| format!("Failed to read device info: {e}"))
                    }),
                    Message::Loaded,
                )
            }
            Message::Loaded(result) => {
                self.loading = false;
                match result {
                    Ok(details) => self.details = Some(details),
                    Err(e) => self.status = Some(StatusMessage::error(e)),
                }
                Task::none()
            }
            Message::Power(action) => {
                let (title, description) = match action {
                    PowerAction::Restart => (
                        t!("utilities_device_restart"),
                        t!("utilities_device_restart_confirm"),
                    ),
                    PowerAction::Shutdown => (
                        t!("utilities_device_shutdown"),
                        t!("utilities_device_shutdown_confirm"),
                    ),
                };
                Task::perform(
                    confirm(title.to_string(), description.to_string()),
                    move |yes| Message::PowerConfirmed(action, yes),
                )
            }
            Message::PowerConfirmed(action, true) => {
                let Some(device) = device.cloned() else {
                    return Task::none();
                };
                Task::perform(
                    run_on_tokio(async move {
                        match action {
                            PowerAction::Restart => device.restart().await,
                            PowerAction::Shutdown => device.shutdown().await,
                        }
                        .map_err(|e| format!("Failed to send power command: {e}"))
                    }),
                    Message::PowerDone,
                )
            }
            Message::PowerConfirmed(_, false) => Task::none(),
            Message::PowerDone(result) => {
                self.status = Some(match result {
                    Ok(()) => StatusMessage::success(t!("utilities_device_power_sent")),
                    Err(e) => StatusMessage::error(e),
                });
                Task::none()
            }
        }
    }

    /// Loads device info the first time the tab is opened.
    pub fn on_open(&self) -> Task<Message> {
        if self.details.is_some() || self.loading {
            Task::none()
        } else {
            Task::done(Message::Refresh)
        }
    }

    pub fn view(&self, usable: bool) -> Element<'_, Message> {
        let small = appearance::THEME_FONT_SIZE - 1.0;
        let unknown = || "—".to_string();

        let refresh_label = if self.loading {
            t!("utilities_loading")
        } else {
            t!("refresh")
        };
        let toolbar = row![
            button(text(refresh_label).align_x(Center))
                .on_press_maybe((usable && !self.loading).then_some(Message::Refresh))
                .style(appearance::s_button),
            container(text("")).width(Fill),
            button(text(t!("utilities_device_restart")).align_x(Center))
                .on_press_maybe(usable.then_some(Message::Power(PowerAction::Restart)))
                .style(appearance::s_button),
            button(text(t!("utilities_device_shutdown")).align_x(Center))
                .on_press_maybe(usable.then_some(Message::Power(PowerAction::Shutdown)))
                .style(appearance::s_button),
        ]
        .spacing(appearance::THEME_PADDING)
        .align_y(Center);

        let mut content = column![toolbar].spacing(appearance::THEME_PADDING);
        if let Some(ref status) = self.status {
            content = content.push(text(&status.text).size(small).color(status.color()));
        }
        if !usable {
            return content.into();
        }

        if let Some(ref d) = self.details {
            let version = match (&d.ios_version, &d.build_version) {
                (Some(v), Some(b)) => format!("{v} ({b})"),
                (Some(v), None) => v.clone(),
                _ => unknown(),
            };
            let battery = match (d.battery_level, d.battery_charging) {
                (Some(level), Some(true)) => {
                    t!("utilities_device_battery_charging", level = level).to_string()
                }
                (Some(level), _) => format!("{level}%"),
                _ => unknown(),
            };
            let storage = match (d.storage_free, d.storage_total) {
                (Some(free), Some(total)) => t!(
                    "utilities_device_storage_value",
                    free = format_bytes(free),
                    total = format_bytes(total)
                )
                .to_string(),
                _ => unknown(),
            };
            let field = |label: String, value: String| {
                row![
                    text(label).size(small).width(120),
                    text(value).size(small).font(appearance::p_font()),
                ]
                .spacing(appearance::THEME_PADDING)
            };
            content = content.push(
                column![
                    field(
                        t!("utilities_device_name").to_string(),
                        d.name.clone().unwrap_or_else(unknown)
                    ),
                    field(
                        t!("utilities_device_model").to_string(),
                        d.product_type.clone().unwrap_or_else(unknown)
                    ),
                    field(t!("utilities_device_ios").to_string(), version),
                    field(
                        t!("utilities_device_serial").to_string(),
                        d.serial_number.clone().unwrap_or_else(unknown)
                    ),
                    field(t!("utilities_device_battery").to_string(), battery),
                    field(t!("utilities_device_storage").to_string(), storage),
                ]
                .spacing(4),
            );
        }

        content.height(Fill).into()
    }
}

fn format_bytes(bytes: u64) -> String {
    const GB: f64 = 1_000_000_000.0;
    format!("{:.1} GB", bytes as f64 / GB)
}
