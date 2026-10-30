//! Browses the container of one installed app (Documents, Library, tmp) and
//! copies files from it to the computer. Opened from an app's Files button.

use iced::widget::{button, column, row, scrollable, text};
use iced::{Center, Color, Element, Fill, Task};
use plume_utils::Device;
use rust_i18n::t;

use super::StatusMessage;
use crate::appearance;
use crate::subscriptions::run_on_tokio;

const START_FOLDER: &str = "/Documents";

#[derive(Debug, Clone)]
pub enum Message {
    Refresh,
    Loaded(String, Result<Vec<String>, String>),
    Open(String),
    Up,
    Select(String),
    Save,
    Saved(Result<bool, String>),
}

#[derive(Debug, Clone, Default)]
pub struct AppFilesTab {
    /// Bundle identifier and display name of the app being browsed.
    app: Option<(String, String)>,
    /// Folder shown, from the container root, without a trailing `/`.
    folder: String,
    /// Names in `folder`; subfolders end in `/`.
    entries: Vec<String>,
    loaded: bool,
    busy: bool,
    selected: Option<String>,
    status: Option<StatusMessage>,
}

impl AppFilesTab {
    /// Switches to `bundle_id`, starting in its Documents folder.
    pub fn open_app(&mut self, bundle_id: String, name: String) -> Task<Message> {
        if self.app.as_ref().is_some_and(|(id, _)| *id == bundle_id) {
            return if self.loaded || self.busy {
                Task::none()
            } else {
                Task::done(Message::Refresh)
            };
        }
        *self = Self {
            app: Some((bundle_id, name)),
            folder: START_FOLDER.to_owned(),
            ..Self::default()
        };
        Task::done(Message::Refresh)
    }

    pub fn update(&mut self, device: Option<&Device>, message: Message) -> Task<Message> {
        match message {
            Message::Refresh => self.load(device),
            Message::Loaded(folder, result) => {
                // A reply for a folder we already left is stale.
                if folder != self.folder {
                    return Task::none();
                }
                self.busy = false;
                self.loaded = true;
                match result {
                    Ok(entries) => {
                        if self
                            .selected
                            .as_ref()
                            .is_some_and(|selected| !entries.contains(selected))
                        {
                            self.selected = None;
                        }
                        self.entries = entries;
                        self.status = None;
                    }
                    Err(e) => {
                        self.entries.clear();
                        self.status = Some(StatusMessage::error(e));
                    }
                }
                Task::none()
            }
            Message::Open(name) => {
                self.folder = format!("{}/{}", self.folder, name.trim_end_matches('/'));
                self.selected = None;
                self.load(device)
            }
            Message::Up => {
                let Some((parent, _)) = self.folder.rsplit_once('/') else {
                    return Task::none();
                };
                self.folder = parent.to_owned();
                self.selected = None;
                self.load(device)
            }
            Message::Select(name) => {
                self.selected = Some(name);
                Task::none()
            }
            Message::Save => {
                let (Some(device), Some((bundle_id, _)), Some(name)) =
                    (device.cloned(), self.app.clone(), self.selected.clone())
                else {
                    return Task::none();
                };
                self.busy = true;
                let path = format!("{}/{}", self.folder, name);
                Task::perform(
                    async move {
                        let Some(handle) = rfd::AsyncFileDialog::new()
                            .set_file_name(&name)
                            .save_file()
                            .await
                        else {
                            return Ok(false);
                        };
                        let data = run_on_tokio(async move {
                            device.read_app_document(&bundle_id, &path).await
                        })
                        .await
                        .map_err(|e| format!("Failed to read {name}: {e}"))?;
                        std::fs::write(handle.path(), data)
                            .map(|_| true)
                            .map_err(|e| format!("Failed to save {name}: {e}"))
                    },
                    Message::Saved,
                )
            }
            Message::Saved(result) => {
                self.busy = false;
                match result {
                    Ok(true) => {
                        self.status = Some(StatusMessage::success(t!("utilities_files_saved")))
                    }
                    Ok(false) => {}
                    Err(e) => self.status = Some(StatusMessage::error(e)),
                }
                Task::none()
            }
        }
    }

    fn load(&mut self, device: Option<&Device>) -> Task<Message> {
        let (Some(device), Some((bundle_id, _))) = (device.cloned(), self.app.clone()) else {
            return Task::none();
        };
        self.busy = true;
        let folder = self.folder.clone();
        Task::perform(
            run_on_tokio({
                let folder = folder.clone();
                async move {
                    device
                        .list_app_documents(&bundle_id, &folder)
                        .await
                        .map_err(|e| format!("Failed to list {folder}: {e}"))
                }
            }),
            move |result| Message::Loaded(folder.clone(), result),
        )
    }

    pub fn view(&self, usable: bool) -> Element<'_, Message> {
        let small = appearance::THEME_FONT_SIZE - 1.0;
        let hint = |label: String| {
            text(label)
                .size(small)
                .color(Color::from_rgb(0.7, 0.7, 0.7))
        };

        if !usable {
            return hint(t!("utilities_no_device_connected").to_string()).into();
        }
        let Some((bundle_id, name)) = &self.app else {
            return hint(t!("utilities_files_pick_app").to_string()).into();
        };

        let refresh_label = if self.busy {
            t!("utilities_loading")
        } else {
            t!("refresh")
        };
        let toolbar = row![
            button(text(t!("utilities_files_up")).align_x(Center))
                .on_press_maybe((!self.busy && !self.folder.is_empty()).then_some(Message::Up))
                .style(appearance::s_button),
            button(text(refresh_label).align_x(Center))
                .on_press_maybe((!self.busy).then_some(Message::Refresh))
                .style(appearance::s_button),
            text(if self.folder.is_empty() {
                "/"
            } else {
                &self.folder
            })
            .font(appearance::p_font())
            .width(Fill),
            button(text(t!("utilities_files_save")).align_x(Center))
                .on_press_maybe((!self.busy && self.selected.is_some()).then_some(Message::Save),)
                .style(appearance::p_button),
        ]
        .spacing(appearance::THEME_PADDING)
        .align_y(Center);

        let mut content = column![text(format!("{name} ({bundle_id})")).size(small), toolbar]
            .spacing(appearance::THEME_PADDING);
        if let Some(ref status) = self.status {
            content = content.push(text(&status.text).size(small).color(status.color()));
        }
        if !self.loaded {
            return content.into();
        }
        if self.entries.is_empty() && self.status.is_none() {
            return content
                .push(text(t!("utilities_files_empty")).size(small))
                .into();
        }

        let list = self
            .entries
            .iter()
            .fold(column![].spacing(2), |col, entry| {
                let is_folder = entry.ends_with('/');
                let selected = self.selected.as_ref() == Some(entry);
                col.push(
                    button(text(entry).size(small).font(appearance::p_font()))
                        .on_press(if is_folder {
                            Message::Open(entry.clone())
                        } else {
                            Message::Select(entry.clone())
                        })
                        .style(if selected {
                            appearance::p_button
                        } else {
                            appearance::s_button
                        })
                        .width(Fill),
                )
            });

        content
            .push(scrollable(list).height(Fill).width(Fill))
            .height(Fill)
            .into()
    }
}
