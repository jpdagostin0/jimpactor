//! Browses the container of one installed app (Documents, Library, tmp), copies
//! files from it to the computer, and backs its data up or restores it. Opened from
//! an app's Files button.

use iced::widget::{button, column, row, scrollable, text};
use iced::{Center, Element, Fill, Task};
use plume_utils::{ArchiveScope, Device, SignerAppReal, TransferSummary, read_backup_manifest};
use rust_i18n::t;
use std::path::PathBuf;

use super::StatusMessage;
use crate::appearance;
use crate::subscriptions::{Work, confirm, run_on_tokio, work_stream};

const START_FOLDER: &str = "/Documents";

#[derive(Debug, Clone)]
pub enum Message {
    /// Handled by the Apps section, which returns to the app list.
    Back,
    Refresh,
    Loaded(String, Result<Vec<String>, String>),
    Open(String),
    Up,
    Select(String),
    Save,
    Saved(Result<bool, String>),
    Backup,
    BackupTo(Option<PathBuf>),
    ExportAll,
    ExportTo(Option<PathBuf>),
    Restore,
    RestoreFrom(Option<PathBuf>),
    RestoreConfirmed(PathBuf),
    Transfer(Work<Result<TransferSummary, String>>),
}

#[derive(Debug, Clone, Default)]
pub struct AppFiles {
    /// Bundle identifier and display name of the app being browsed.
    app: Option<(String, String)>,
    version: Option<String>,
    /// Folder shown, from the container root, without a trailing `/`.
    folder: String,
    /// Names in `folder`; subfolders end in `/`.
    entries: Vec<String>,
    loaded: bool,
    busy: bool,
    selected: Option<String>,
    status: Option<StatusMessage>,
}

impl AppFiles {
    /// Switches to `bundle_id`, starting in its Documents folder.
    pub fn open_app(&mut self, app: &SignerAppReal) -> Task<Message> {
        let Some(bundle_id) = app.bundle_id.clone() else {
            return Task::none();
        };
        let name = app
            .installed_name
            .clone()
            .unwrap_or_else(|| app.app.to_string());
        if self.app.as_ref().is_some_and(|(id, _)| *id == bundle_id) {
            return if self.loaded || self.busy {
                Task::none()
            } else {
                Task::done(Message::Refresh)
            };
        }
        *self = Self {
            app: Some((bundle_id, name)),
            version: app.version.clone(),
            folder: START_FOLDER.to_owned(),
            ..Self::default()
        };
        Task::done(Message::Refresh)
    }

    pub fn update(&mut self, device: Option<&Device>, message: Message) -> Task<Message> {
        match message {
            Message::Back => Task::none(),
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
            Message::Backup => {
                let Some((bundle_id, _)) = &self.app else {
                    return Task::none();
                };
                let file_name = format!(
                    "{bundle_id}-{}.zip",
                    chrono::Local::now().format("%Y%m%d-%H%M%S")
                );
                Task::perform(
                    async move {
                        rfd::AsyncFileDialog::new()
                            .add_filter("Backup", &["zip"])
                            .set_file_name(file_name)
                            .save_file()
                            .await
                            .map(|file| file.path().to_owned())
                    },
                    Message::BackupTo,
                )
            }
            Message::ExportAll => {
                let Some((bundle_id, _)) = &self.app else {
                    return Task::none();
                };
                let file_name = format!("{bundle_id}-files.zip");
                Task::perform(
                    async move {
                        rfd::AsyncFileDialog::new()
                            .add_filter("Zip", &["zip"])
                            .set_file_name(file_name)
                            .save_file()
                            .await
                            .map(|file| file.path().to_owned())
                    },
                    Message::ExportTo,
                )
            }
            Message::ExportTo(Some(path)) => {
                let (Some(device), Some((bundle_id, _))) = (device.cloned(), self.app.clone())
                else {
                    return Task::none();
                };
                self.begin_transfer(move |progress| async move {
                    device
                        .archive_app_files(
                            &bundle_id,
                            None,
                            ArchiveScope::Everything,
                            &path,
                            move |file| {
                                let _ = progress.send(file.to_owned());
                            },
                        )
                        .await
                        .map_err(|e| format!("Export failed: {e}"))
                })
            }
            Message::BackupTo(None) | Message::ExportTo(None) | Message::RestoreFrom(None) => {
                Task::none()
            }
            Message::BackupTo(Some(path)) => {
                let (Some(device), Some((bundle_id, _))) = (device.cloned(), self.app.clone())
                else {
                    return Task::none();
                };
                let version = self.version.clone();
                self.begin_transfer(move |progress| async move {
                    device
                        .archive_app_files(
                            &bundle_id,
                            version,
                            ArchiveScope::Backup,
                            &path,
                            move |file| {
                                let _ = progress.send(file.to_owned());
                            },
                        )
                        .await
                        .map_err(|e| format!("Backup failed: {e}"))
                })
            }
            Message::Restore => Task::perform(
                async {
                    rfd::AsyncFileDialog::new()
                        .add_filter("Backup", &["zip"])
                        .pick_file()
                        .await
                        .map(|file| file.path().to_owned())
                },
                Message::RestoreFrom,
            ),
            Message::RestoreFrom(Some(path)) => {
                let Some((bundle_id, name)) = self.app.clone() else {
                    return Task::none();
                };
                let manifest = match read_backup_manifest(&path) {
                    Ok(manifest) => manifest,
                    Err(e) => {
                        self.status = Some(StatusMessage::error(e.to_string()));
                        return Task::none();
                    }
                };
                let mut description = t!(
                    "utilities_restore_confirm",
                    files = manifest.files,
                    date = manifest.created.format("%Y-%m-%d %H:%M").to_string(),
                    name = name
                )
                .into_owned();
                if manifest.bundle_id != bundle_id {
                    description.push_str("\n\n");
                    description
                        .push_str(&t!("utilities_restore_other_app", app = manifest.bundle_id));
                }
                let title = t!("utilities_files_restore").into_owned();
                Task::perform(confirm(title, description), move |yes| {
                    if yes {
                        Message::RestoreConfirmed(path.clone())
                    } else {
                        Message::RestoreFrom(None)
                    }
                })
            }
            Message::RestoreConfirmed(path) => {
                let (Some(device), Some((bundle_id, _))) = (device.cloned(), self.app.clone())
                else {
                    return Task::none();
                };
                self.begin_transfer(move |progress| async move {
                    device
                        .restore_app_files(&bundle_id, &path, move |file| {
                            let _ = progress.send(file.to_owned());
                        })
                        .await
                        .map_err(|e| format!("Restore failed: {e}"))
                })
            }
            Message::Transfer(Work::Progress(file)) => {
                self.status = Some(StatusMessage::success(file));
                Task::none()
            }
            Message::Transfer(Work::Done(result)) => {
                self.busy = false;
                self.status = Some(match result {
                    Ok(summary) => StatusMessage::success(t!(
                        "utilities_files_transferred",
                        files = summary.files,
                        size = format!("{:.1}", summary.bytes as f64 / 1_048_576.0),
                        skipped = summary.skipped.len()
                    )),
                    Err(e) => StatusMessage::error(e),
                });
                self.load(device)
            }
        }
    }

    fn begin_transfer<F, Fut>(&mut self, work: F) -> Task<Message>
    where
        F: FnOnce(tokio::sync::mpsc::UnboundedSender<String>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<TransferSummary, String>> + Send + 'static,
    {
        self.busy = true;
        self.status = None;
        Task::stream(work_stream(work)).map(Message::Transfer)
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

    pub fn view(&self) -> Element<'_, Message> {
        let small = appearance::THEME_FONT_SIZE - 1.0;
        let (bundle_id, name) = self
            .app
            .as_ref()
            .map_or(("", ""), |(id, name)| (id.as_str(), name.as_str()));

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

        let header = row![
            button(text(t!("back")).align_x(Center))
                .on_press(Message::Back)
                .style(appearance::s_button),
            text(format!("{name} ({bundle_id})")).width(Fill),
            button(text(t!("utilities_files_backup")).align_x(Center))
                .on_press_maybe((!self.busy).then_some(Message::Backup))
                .style(appearance::s_button),
            button(text(t!("utilities_files_restore")).align_x(Center))
                .on_press_maybe((!self.busy).then_some(Message::Restore))
                .style(appearance::s_button),
            button(text(t!("utilities_files_export_all")).align_x(Center))
                .on_press_maybe((!self.busy).then_some(Message::ExportAll))
                .style(appearance::s_button),
        ]
        .spacing(appearance::THEME_PADDING)
        .align_y(Center);

        let mut content = column![header, toolbar].spacing(appearance::THEME_PADDING);
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
