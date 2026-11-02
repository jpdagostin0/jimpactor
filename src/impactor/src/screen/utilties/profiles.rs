use iced::widget::{button, column, pick_list, row, scrollable, text};
use iced::{Center, Color, Element, Fill, Task};
use plume_utils::{Device, InstalledProfile, ProfileSort, sort_profiles};
use rust_i18n::t;

use super::StatusMessage;
use crate::appearance;
use crate::subscriptions::{confirm, run_on_tokio};

#[derive(Debug, Clone)]
pub enum Message {
    Refresh,
    Loaded(Result<Vec<InstalledProfile>, String>),
    Remove(Vec<String>),
    RemoveConfirmed(Vec<String>, bool),
    Removed(Result<(), String>),
    SelectSort(SortChoice),
    ToggleDirection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortChoice(ProfileSort);

impl std::fmt::Display for SortChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&match self.0 {
            ProfileSort::Expiration => t!("utilities_profiles_sort_expiration"),
            ProfileSort::Name => t!("utilities_profiles_sort_name"),
            ProfileSort::AppId => t!("utilities_profiles_sort_app_id"),
            ProfileSort::Team => t!("utilities_profiles_sort_team"),
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct ProfilesTab {
    profiles: Vec<InstalledProfile>,
    loaded: bool,
    busy: bool,
    status: Option<StatusMessage>,
    sort: ProfileSort,
    descending: bool,
}

impl ProfilesTab {
    pub fn update(&mut self, device: Option<&Device>, message: Message) -> Task<Message> {
        match message {
            Message::Refresh => {
                let Some(device) = device.cloned() else {
                    return Task::none();
                };
                self.busy = true;
                Task::perform(
                    run_on_tokio(async move {
                        device
                            .installed_profiles()
                            .await
                            .map_err(|e| format!("Failed to load profiles: {e}"))
                    }),
                    Message::Loaded,
                )
            }
            Message::Loaded(result) => {
                self.busy = false;
                self.loaded = true;
                match result {
                    Ok(mut profiles) => {
                        sort_profiles(&mut profiles, self.sort, self.descending);
                        self.profiles = profiles;
                        self.status = None;
                    }
                    Err(e) => self.status = Some(StatusMessage::error(e)),
                }
                Task::none()
            }
            Message::Remove(uuids) => {
                if uuids.is_empty() {
                    return Task::none();
                }
                let description = t!("utilities_profiles_remove_confirm", count = uuids.len());
                Task::perform(
                    confirm(
                        t!("utilities_profiles_remove").to_string(),
                        description.to_string(),
                    ),
                    move |yes| Message::RemoveConfirmed(uuids.clone(), yes),
                )
            }
            Message::RemoveConfirmed(uuids, true) => {
                let Some(device) = device.cloned() else {
                    return Task::none();
                };
                self.busy = true;
                Task::perform(
                    run_on_tokio(async move {
                        device
                            .remove_profiles(uuids)
                            .await
                            .map_err(|e| format!("Failed to remove profile: {e}"))
                    }),
                    Message::Removed,
                )
            }
            Message::RemoveConfirmed(_, false) => Task::none(),
            Message::SelectSort(SortChoice(sort)) => {
                self.sort = sort;
                sort_profiles(&mut self.profiles, self.sort, self.descending);
                Task::none()
            }
            Message::ToggleDirection => {
                self.descending = !self.descending;
                sort_profiles(&mut self.profiles, self.sort, self.descending);
                Task::none()
            }
            Message::Removed(result) => {
                self.busy = false;
                match result {
                    Ok(()) => {
                        self.status =
                            Some(StatusMessage::success(t!("utilities_profiles_removed")));
                        Task::done(Message::Refresh)
                    }
                    Err(e) => {
                        self.status = Some(StatusMessage::error(e));
                        Task::done(Message::Refresh)
                    }
                }
            }
        }
    }

    /// Loads the list the first time the tab is opened.
    pub fn on_open(&self) -> Task<Message> {
        if self.loaded || self.busy {
            Task::none()
        } else {
            Task::done(Message::Refresh)
        }
    }

    pub fn view(&self, usable: bool) -> Element<'_, Message> {
        let small = appearance::THEME_FONT_SIZE - 1.0;
        let expired: Vec<String> = self
            .profiles
            .iter()
            .filter(|p| p.is_expired())
            .map(|p| p.uuid.clone())
            .collect();

        let refresh_label = if self.busy {
            t!("utilities_loading")
        } else {
            t!("refresh")
        };
        let toolbar = row![
            button(text(refresh_label).align_x(Center))
                .on_press_maybe((usable && !self.busy).then_some(Message::Refresh))
                .style(appearance::s_button),
            text(t!("utilities_profiles_count", count = self.profiles.len()))
                .size(small)
                .width(Fill),
            pick_list(
                ProfileSort::ALL.map(SortChoice),
                Some(SortChoice(self.sort)),
                Message::SelectSort,
            )
            .style(appearance::s_pick_list),
            button(
                text(if self.descending {
                    t!("utilities_profiles_descending")
                } else {
                    t!("utilities_profiles_ascending")
                })
                .align_x(Center)
            )
            .on_press(Message::ToggleDirection)
            .style(appearance::s_button),
            button(text(t!("utilities_profiles_remove_expired")).align_x(Center))
                .on_press_maybe(
                    (usable && !self.busy && !expired.is_empty())
                        .then(|| Message::Remove(expired.clone())),
                )
                .style(appearance::s_button),
        ]
        .spacing(appearance::THEME_PADDING)
        .align_y(Center);

        let mut content = column![toolbar].spacing(appearance::THEME_PADDING);
        if let Some(ref status) = self.status {
            content = content.push(text(&status.text).size(small).color(status.color()));
        }
        if usable && self.loaded && self.profiles.is_empty() {
            content = content.push(text(t!("utilities_profiles_empty")).size(small));
        }

        let list = self
            .profiles
            .iter()
            .fold(column![].spacing(6), |col, profile| {
                let expiration = match profile.expiration {
                    Some(date) => {
                        let date = chrono::DateTime::<chrono::Local>::from(date)
                            .format("%Y-%m-%d %H:%M")
                            .to_string();
                        if profile.is_expired() {
                            t!("utilities_profiles_expired", date = date)
                        } else {
                            t!("utilities_profiles_expires", date = date)
                        }
                    }
                    None => t!("utilities_profiles_no_expiration"),
                };
                let mut details = profile.app_id.clone().unwrap_or_default();
                if let Some(team) = &profile.team_name {
                    details = format!("{details} · {team}");
                }
                let mut expiration_text = text(expiration).size(small);
                if profile.is_expired() {
                    expiration_text = expiration_text.color(Color::from_rgb(0.9, 0.35, 0.35));
                }
                col.push(
                    row![
                        column![
                            text(&profile.name).size(14),
                            text(details).size(small),
                            expiration_text,
                        ]
                        .spacing(2)
                        .width(Fill),
                        button(text(t!("utilities_profiles_remove")).align_x(Center))
                            .on_press_maybe(
                                (!self.busy).then(|| Message::Remove(vec![profile.uuid.clone()])),
                            )
                            .style(appearance::s_button),
                    ]
                    .spacing(appearance::THEME_PADDING)
                    .align_y(Center),
                )
            });

        content
            .push(scrollable(list).height(Fill).width(Fill))
            .height(Fill)
            .into()
    }
}
