//! The device's crash logs, as listed under Settings → Privacy & Security →
//! Analytics & Improvements → Analytics Data.

use iced::widget::{button, column, row, scrollable, text, text_input};
use iced::{Center, Element, Fill, Task};
use plume_utils::Device;
use rust_i18n::t;

use super::StatusMessage;
use crate::appearance;
use crate::subscriptions::run_on_tokio;

// Drawing thousands of rows gets slow; the search narrows it down.
const LOGS_SHOWN: usize = 500;

#[derive(Debug, Clone)]
pub enum Message {
    Refresh,
    Loaded(Result<Vec<String>, String>),
    SearchChanged(String),
    Select(String),
    Save,
    Saved(Result<bool, String>),
    SaveAll,
    SavedAll(Result<Option<usize>, String>),
}

#[derive(Debug, Clone, Default)]
pub struct CrashLogsTab {
    /// Paths relative to the crash log folder, newest first.
    logs: Vec<String>,
    loaded: bool,
    busy: bool,
    search: String,
    selected: Option<String>,
    status: Option<StatusMessage>,
}

impl CrashLogsTab {
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
                            .crash_reports()
                            .await
                            .map_err(|e| format!("Failed to list crash logs: {e}"))
                    }),
                    Message::Loaded,
                )
            }
            Message::Loaded(result) => {
                self.busy = false;
                self.loaded = true;
                match result {
                    Ok(logs) => {
                        if self
                            .selected
                            .as_ref()
                            .is_some_and(|selected| !logs.contains(selected))
                        {
                            self.selected = None;
                        }
                        self.logs = logs;
                        self.status = None;
                    }
                    Err(e) => self.status = Some(StatusMessage::error(e)),
                }
                Task::none()
            }
            Message::SearchChanged(search) => {
                self.search = search;
                Task::none()
            }
            Message::Select(path) => {
                self.selected = Some(path);
                Task::none()
            }
            Message::Save => {
                let (Some(device), Some(path)) = (device.cloned(), self.selected.clone()) else {
                    return Task::none();
                };
                self.busy = true;
                let file_name = file_name(&path).to_owned();
                Task::perform(
                    async move {
                        let Some(handle) = rfd::AsyncFileDialog::new()
                            .set_file_name(file_name)
                            .save_file()
                            .await
                        else {
                            return Ok(false);
                        };
                        let data = run_on_tokio(async move { device.crash_report(&path).await })
                            .await
                            .map_err(|e| format!("Failed to read crash log: {e}"))?;
                        std::fs::write(handle.path(), data)
                            .map(|_| true)
                            .map_err(|e| format!("Failed to save crash log: {e}"))
                    },
                    Message::Saved,
                )
            }
            Message::Saved(result) => {
                self.busy = false;
                match result {
                    Ok(true) => {
                        self.status = Some(StatusMessage::success(t!("utilities_crash_logs_saved")))
                    }
                    Ok(false) => {}
                    Err(e) => self.status = Some(StatusMessage::error(e)),
                }
                Task::none()
            }
            Message::SaveAll => {
                let Some(device) = device.cloned() else {
                    return Task::none();
                };
                let logs: Vec<String> = self.matching().into_iter().cloned().collect();
                if logs.is_empty() {
                    return Task::none();
                }
                self.busy = true;
                Task::perform(
                    async move {
                        let Some(folder) = rfd::AsyncFileDialog::new().pick_folder().await else {
                            return Ok(None);
                        };
                        let folder = folder.path().to_owned();
                        run_on_tokio(async move {
                            device
                                .export_crash_reports(logs, &folder)
                                .await
                                .map(Some)
                                .map_err(|e| format!("Failed to export crash logs: {e}"))
                        })
                        .await
                    },
                    Message::SavedAll,
                )
            }
            Message::SavedAll(result) => {
                self.busy = false;
                match result {
                    Ok(Some(count)) => {
                        self.status = Some(StatusMessage::success(t!(
                            "utilities_crash_logs_exported",
                            count = count
                        )))
                    }
                    Ok(None) => {}
                    Err(e) => self.status = Some(StatusMessage::error(e)),
                }
                Task::none()
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

    /// Logs whose file name fuzzy-matches the search, best match first. Equal
    /// scores keep the newest-first order.
    fn matching(&self) -> Vec<&String> {
        let query = self.search.trim();
        if query.is_empty() {
            return self.logs.iter().collect();
        }
        let mut scored: Vec<(i32, &String)> = self
            .logs
            .iter()
            .filter_map(|path| fuzzy_score(query, file_name(path)).map(|score| (score, path)))
            .collect();
        scored.sort_by_key(|&(score, _)| std::cmp::Reverse(score));
        scored.into_iter().map(|(_, path)| path).collect()
    }

    pub fn view(&self, usable: bool) -> Element<'_, Message> {
        let small = appearance::THEME_FONT_SIZE - 1.0;
        let matching = self.matching();

        let refresh_label = if self.busy {
            t!("utilities_loading")
        } else {
            t!("refresh")
        };
        let toolbar = row![
            button(text(refresh_label).align_x(Center))
                .on_press_maybe((usable && !self.busy).then_some(Message::Refresh))
                .style(appearance::s_button),
            text_input(&t!("utilities_crash_logs_search"), &self.search)
                .on_input(Message::SearchChanged)
                .width(Fill),
            button(text(t!("utilities_crash_logs_save")).align_x(Center))
                .on_press_maybe(
                    (usable && !self.busy && self.selected.is_some()).then_some(Message::Save),
                )
                .style(appearance::p_button),
            button(text(t!("utilities_crash_logs_save_all")).align_x(Center))
                .on_press_maybe(
                    (usable && !self.busy && !matching.is_empty()).then_some(Message::SaveAll),
                )
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
        if !self.loaded {
            return content.into();
        }
        if self.logs.is_empty() {
            return content
                .push(text(t!("utilities_crash_logs_empty")).size(small))
                .into();
        }

        let count = if self.search.trim().is_empty() {
            t!("utilities_crash_logs_count", count = self.logs.len())
        } else {
            t!(
                "utilities_crash_logs_matching",
                count = matching.len(),
                total = self.logs.len()
            )
        };
        content = content.push(text(count).size(small));

        let list = matching
            .into_iter()
            .take(LOGS_SHOWN)
            .fold(column![].spacing(2), |col, path| {
                let name = file_name(path);
                let folder = path.strip_suffix(name).map(|f| f.trim_end_matches('/'));
                let mut label = row![
                    text(name)
                        .size(small)
                        .font(appearance::p_font())
                        .width(Fill)
                ]
                .spacing(appearance::THEME_PADDING)
                .align_y(Center);
                if let Some(folder) = folder.filter(|f| !f.is_empty()) {
                    label = label.push(text(folder).size(small));
                }
                let selected = self.selected.as_ref() == Some(path);
                col.push(
                    button(label)
                        .on_press(Message::Select(path.clone()))
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

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Case-insensitive subsequence match. Higher is better; `None` when some query
/// character is missing. Consecutive runs and matches at word starts score
/// higher, so "webcont" ranks `WebContent-...` above a scattered match.
fn fuzzy_score(query: &str, candidate: &str) -> Option<i32> {
    let candidate: Vec<char> = candidate.chars().collect();
    let mut score = 0;
    let mut next = 0;
    let mut previous_match: Option<usize> = None;
    for wanted in query.chars().filter(|c| !c.is_whitespace()) {
        let wanted = wanted.to_lowercase().next().unwrap_or(wanted);
        let found = (next..candidate.len())
            .find(|&i| candidate[i].to_lowercase().next() == Some(wanted))?;

        score += 1;
        let at_word_start = found == 0 || {
            let before = candidate[found - 1];
            !before.is_alphanumeric() || (before.is_lowercase() && candidate[found].is_uppercase())
        };
        if at_word_start {
            score += 8;
        }
        match previous_match {
            Some(prev) if prev + 1 == found => score += 5,
            Some(prev) => score -= (found - prev - 1).min(5) as i32,
            None => score -= found.min(10) as i32,
        }
        previous_match = Some(found);
        next = found + 1;
    }
    Some(score)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_matches_subsequences_only() {
        assert!(fuzzy_score("wbcnt", "WebContent-2026-01-02-101010.ips").is_some());
        assert!(fuzzy_score("WEB", "com.apple.WebKit.ips").is_some());
        assert!(fuzzy_score("xyz", "WebContent.ips").is_none());
        assert!(fuzzy_score("tnetnoc", "WebContent.ips").is_none());
    }

    #[test]
    fn fuzzy_prefers_tight_matches() {
        let tight = fuzzy_score("jetsam", "JetsamEvent-2026-01-02-101010.ips").unwrap();
        let loose = fuzzy_score("jetsam", "Jxextxsxaxm-2026-01-02-101010.ips").unwrap();
        assert!(tight > loose);
    }

    #[test]
    fn file_name_drops_folder() {
        assert_eq!(file_name("Retired/Foo.ips"), "Foo.ips");
        assert_eq!(file_name("Foo.ips"), "Foo.ips");
    }
}
