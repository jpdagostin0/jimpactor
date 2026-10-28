use iced::widget::{button, column, pick_list, row, text};
use iced::{Alignment, Element, Fill, Task};
use rust_i18n::t;

use crate::appearance;
use crate::sources::{self, AppVersion, SourceApp, SourcesStore};
use crate::subscriptions::run_on_tokio;

#[derive(Debug, Clone)]
pub enum Message {
    Load,
    Loaded(Vec<SourceApp>),
    SelectApp(SourceApp),
    SelectVersion(AppVersion),
    /// Handled by the app, which starts the installation.
    Install(SourceApp, AppVersion),
}

/// One-click install of apps from the configured sources, SideStore preselected.
#[derive(Debug, Default)]
pub struct QuickInstall {
    apps: Vec<SourceApp>,
    selected_app: Option<SourceApp>,
    selected_version: Option<AppVersion>,
    loading: bool,
}

impl QuickInstall {
    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Load => {
                if self.loading {
                    return Task::none();
                }
                self.loading = true;
                let sources = SourcesStore::load(&crate::defaults::get_data_path())
                    .sources()
                    .to_vec();
                Task::perform(run_on_tokio(sources::fetch_all(sources)), Message::Loaded)
            }
            Message::Loaded(apps) => {
                self.loading = false;
                self.apps = apps;

                let keep = self.selected_app.as_ref().and_then(|selected| {
                    self.apps
                        .iter()
                        .find(|a| a.name == selected.name && a.source_name == selected.source_name)
                });
                let app = keep
                    .or_else(|| self.apps.iter().find(|a| a.name == sources::SIDESTORE))
                    .or_else(|| self.apps.first())
                    .cloned();
                match app {
                    Some(app) => return self.update(Message::SelectApp(app)),
                    None => {
                        self.selected_app = None;
                        self.selected_version = None;
                    }
                }
                Task::none()
            }
            Message::SelectApp(app) => {
                let keep_version = self
                    .selected_version
                    .as_ref()
                    .filter(|v| app.versions.contains(v))
                    .cloned();
                self.selected_version = keep_version.or_else(|| app.default_version().cloned());
                self.selected_app = Some(app);
                Task::none()
            }
            Message::SelectVersion(version) => {
                self.selected_version = Some(version);
                Task::none()
            }
            Message::Install(..) => Task::none(),
        }
    }

    pub fn view(&self, has_device: bool) -> Element<'_, Message> {
        let app_placeholder = if self.loading {
            t!("quick_install_loading")
        } else {
            t!("quick_install_no_apps")
        };

        let mut app_pick = pick_list(
            self.apps.clone(),
            self.selected_app.clone(),
            Message::SelectApp,
        )
        .placeholder(app_placeholder)
        .style(appearance::s_pick_list)
        .width(Fill);
        // Retry when the first load failed (e.g. offline), without hammering the GitHub API.
        if self.apps.is_empty() {
            app_pick = app_pick.on_open(Message::Load);
        }

        let versions = self
            .selected_app
            .as_ref()
            .map(|a| a.versions.clone())
            .unwrap_or_default();
        let version_pick = pick_list(
            versions,
            self.selected_version.clone(),
            Message::SelectVersion,
        )
        .placeholder(t!("quick_install_version"))
        .style(appearance::s_pick_list)
        .width(190);

        let install = match (&self.selected_app, &self.selected_version) {
            (Some(app), Some(version)) if has_device && !self.loading => {
                Some(Message::Install(app.clone(), version.clone()))
            }
            _ => None,
        };
        let install_button = button(appearance::icon_text(
            appearance::DOWNLOAD,
            t!("install"),
            None,
        ))
        .on_press_maybe(install)
        .style(appearance::p_button);

        let mut content = column![
            row![app_pick, version_pick, install_button]
                .spacing(appearance::THEME_PADDING)
                .align_y(Alignment::Center),
        ]
        .spacing(appearance::THEME_PADDING);
        if !has_device {
            content = content.push(
                text(t!("utilities_no_device_connected")).size(appearance::THEME_FONT_SIZE - 1.0),
            );
        }
        content.into()
    }
}
