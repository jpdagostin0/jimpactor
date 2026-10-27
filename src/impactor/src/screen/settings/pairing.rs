use std::path::PathBuf;

use iced::widget::{button, checkbox, column, container, row, rule, scrollable, text};
use iced::{Alignment, Element, Fill, Task};
use plume_utils::{Device, REMOTE_PAIRING_CACHE_PREFIX, REMOTE_PAIRING_CACHE_SUFFIX};
use rust_i18n::t;

use crate::appearance;
use crate::defaults::get_data_path;
use crate::subscriptions::{confirm, run_on_tokio};

#[derive(Debug, Clone)]
pub enum Message {
    Refresh,
    /// Handled by the app, which owns the setting.
    ToggleRemotePairing(bool),
    ExportForDevice(Device),
    ExportCached(CachedPairing),
    DeleteCached(CachedPairing),
    Done(Result<Option<String>, String>),
}

/// A remote pairing (RPPairing) record cached on disk for a device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedPairing {
    pub udid: String,
    pub path: PathBuf,
}

#[derive(Debug, Default)]
pub struct PairingTab {
    cached: Vec<CachedPairing>,
    busy: bool,
    status: Option<Result<String, String>>,
}

fn list_cached() -> Vec<CachedPairing> {
    let Ok(entries) = std::fs::read_dir(get_data_path()) else {
        return Vec::new();
    };
    let mut cached: Vec<_> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let udid = name
                .strip_prefix(REMOTE_PAIRING_CACHE_PREFIX)?
                .strip_suffix(REMOTE_PAIRING_CACHE_SUFFIX)?
                .to_string();
            // UDIDs are 25 (hyphenated) or 40 hex characters.
            let looks_like_udid =
                udid.len() >= 24 && udid.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
            looks_like_udid.then(|| CachedPairing {
                udid,
                path: e.path(),
            })
        })
        .collect();
    cached.sort_by(|a, b| a.udid.cmp(&b.udid));
    cached
}

async fn save_as(default_name: String, data: Vec<u8>) -> Result<Option<String>, String> {
    let Some(file) = rfd::AsyncFileDialog::new()
        .set_title(t!("pairing_export"))
        .set_file_name(default_name)
        .save_file()
        .await
    else {
        return Ok(None);
    };
    tokio::fs::write(file.path(), data)
        .await
        .map_err(|e| e.to_string())?;
    Ok(Some(
        t!("pairing_exported", path = file.path().display().to_string()).to_string(),
    ))
}

impl PairingTab {
    pub fn new() -> Self {
        Self {
            cached: list_cached(),
            ..Self::default()
        }
    }

    pub fn update(&mut self, message: Message, remote_pairing: bool) -> Task<Message> {
        match message {
            Message::Refresh => {
                self.cached = list_cached();
                Task::none()
            }
            Message::ToggleRemotePairing(_) => Task::none(),
            Message::ExportForDevice(device) => {
                self.busy = true;
                self.status = Some(Ok(t!("pairing_generating").to_string()));
                Task::perform(
                    run_on_tokio(async move {
                        let data = device
                            .pairing_file(remote_pairing, get_data_path())
                            .await
                            .map_err(|e| e.to_string())?;
                        save_as(format!("{}.mobiledevicepairing", device.udid), data).await
                    }),
                    Message::Done,
                )
            }
            Message::ExportCached(cached) => {
                self.busy = true;
                Task::perform(
                    run_on_tokio(async move {
                        let data = tokio::fs::read(&cached.path)
                            .await
                            .map_err(|e| e.to_string())?;
                        save_as(format!("{}_rppairing.plist", cached.udid), data).await
                    }),
                    Message::Done,
                )
            }
            Message::DeleteCached(cached) => {
                self.busy = true;
                Task::perform(
                    async move {
                        if !confirm(
                            t!("pairing_delete").to_string(),
                            t!("pairing_delete_confirm", udid = cached.udid.clone()).to_string(),
                        )
                        .await
                        {
                            return Ok(None);
                        }
                        std::fs::remove_file(&cached.path).map_err(|e| e.to_string())?;
                        log::info!("Deleted cached remote pairing for {}", cached.udid);
                        Ok(Some(t!("pairing_deleted").to_string()))
                    },
                    Message::Done,
                )
            }
            Message::Done(result) => {
                self.busy = false;
                self.cached = list_cached();
                match result {
                    Ok(Some(msg)) => self.status = Some(Ok(msg)),
                    Ok(None) => self.status = None,
                    Err(e) => {
                        log::error!("Pairing file operation failed: {e}");
                        self.status = Some(Err(e));
                    }
                }
                Task::none()
            }
        }
    }

    pub fn view<'a>(
        &'a self,
        remote_pairing: bool,
        selected_device: Option<&'a Device>,
        devices: &'a [Device],
    ) -> Element<'a, Message> {
        let mut content = column![
            checkbox(remote_pairing)
                .label(t!("pairing_include_remote"))
                .on_toggle(Message::ToggleRemotePairing),
            text(t!("pairing_include_remote_desc")).size(appearance::THEME_FONT_SIZE - 1.0),
        ]
        .spacing(appearance::THEME_PADDING);

        let export = selected_device
            .filter(|d| !d.is_mac && !self.busy)
            .map(|d| Message::ExportForDevice(d.clone()));
        let export_label = match selected_device {
            Some(device) => t!("pairing_export_for", name = device.name.clone()),
            None => t!("utilities_no_device_connected"),
        };
        content = content.push(
            button(appearance::icon_text(appearance::SHARE, export_label, None))
                .on_press_maybe(export)
                .style(appearance::s_button),
        );

        if let Some(status) = &self.status {
            let (msg, color) = match status {
                Ok(msg) => (msg, iced::Color::from_rgb(0.2, 0.8, 0.4)),
                Err(msg) => (msg, iced::Color::from_rgb(0.9, 0.2, 0.2)),
            };
            content = content.push(text(msg).size(appearance::THEME_FONT_SIZE).color(color));
        }

        content = content
            .push(container(rule::horizontal(1)).padding([appearance::THEME_PADDING / 2.0, 0.0]))
            .push(
                row![
                    text(t!("pairing_cached_title")).width(Fill),
                    button(text(t!("refresh")))
                        .on_press(Message::Refresh)
                        .style(appearance::s_button),
                ]
                .align_y(Alignment::Center),
            );

        if self.cached.is_empty() {
            content = content
                .push(text(t!("pairing_cached_none")).size(appearance::THEME_FONT_SIZE - 1.0));
        }

        for cached in &self.cached {
            let label = devices
                .iter()
                .find(|d| d.udid == cached.udid)
                .map(|d| format!("{} ({})", d.name, cached.udid))
                .unwrap_or_else(|| cached.udid.clone());
            let enabled = !self.busy;
            content = content.push(
                row![
                    text(label).size(appearance::THEME_FONT_SIZE).width(Fill),
                    button(text(t!("export")))
                        .on_press_maybe(enabled.then(|| Message::ExportCached(cached.clone())))
                        .style(appearance::s_button),
                    button(text(t!("pairing_delete")))
                        .on_press_maybe(enabled.then(|| Message::DeleteCached(cached.clone())))
                        .style(appearance::s_button),
                ]
                .spacing(appearance::THEME_PADDING)
                .align_y(Alignment::Center),
            );
        }

        scrollable(content).height(Fill).into()
    }
}
