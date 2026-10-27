use iced::widget::{button, column, container, pick_list, row, scrollable, text};
use iced::{Alignment, Element, Fill, Task};
use log::LevelFilter;
use rust_i18n::t;

use crate::appearance;
use crate::logging;

// Drawing every buffered entry as a widget gets slow; the file has the rest.
const MAX_SHOWN: usize = 400;

#[derive(Debug, Clone)]
pub enum Message {
    /// Periodic redraw while the tab is open, so new entries show up.
    Tick,
    SelectLevel(LevelChoice),
    Copy,
    Clear,
    OpenFolder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LevelChoice(LevelFilter);

impl LevelChoice {
    const ALL: [LevelChoice; 4] = [
        LevelChoice(LevelFilter::Error),
        LevelChoice(LevelFilter::Warn),
        LevelChoice(LevelFilter::Info),
        LevelChoice(LevelFilter::Debug),
    ];
}

impl std::fmt::Display for LevelChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug)]
pub struct LogsTab {
    level: LevelChoice,
}

impl Default for LogsTab {
    fn default() -> Self {
        Self {
            level: LevelChoice(LevelFilter::Info),
        }
    }
}

impl LogsTab {
    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Tick => Task::none(),
            Message::SelectLevel(level) => {
                self.level = level;
                Task::none()
            }
            Message::Copy => {
                let contents = logging::entries(self.level.0)
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("\n");
                iced::clipboard::write(contents)
            }
            Message::Clear => {
                logging::clear();
                Task::none()
            }
            Message::OpenFolder => {
                if let Err(e) = open::that(logging::log_dir()) {
                    log::error!("Failed to open log folder: {e}");
                }
                Task::none()
            }
        }
    }

    pub fn view(&self) -> Element<'_, Message> {
        let entries = logging::entries(self.level.0);
        let small = appearance::THEME_FONT_SIZE - 1.0;

        let lines = if entries.is_empty() {
            column![text(t!("logs_empty")).size(small)]
        } else {
            entries.iter().rev().take(MAX_SHOWN).fold(column![].spacing(2), |col, entry| {
                let color = match entry.level {
                    log::Level::Error => Some(iced::Color::from_rgb(0.9, 0.35, 0.35)),
                    log::Level::Warn => Some(iced::Color::from_rgb(0.95, 0.75, 0.4)),
                    _ => None,
                };
                let mut line = text(entry.to_string()).size(small);
                if let Some(color) = color {
                    line = line.color(color);
                }
                col.push(line)
            })
        };

        let toolbar = row![
            pick_list(LevelChoice::ALL, Some(self.level), Message::SelectLevel)
                .style(appearance::s_pick_list),
            container(text("")).width(Fill),
            button(text(t!("logs_copy")))
                .on_press(Message::Copy)
                .style(appearance::s_button),
            button(text(t!("logs_clear")))
                .on_press(Message::Clear)
                .style(appearance::s_button),
            button(text(t!("logs_open_folder")))
                .on_press(Message::OpenFolder)
                .style(appearance::s_button),
        ]
        .spacing(appearance::THEME_PADDING)
        .align_y(Alignment::Center);

        column![
            toolbar,
            text(t!("logs_newest_first")).size(small),
            scrollable(lines).height(Fill).width(Fill),
        ]
        .spacing(appearance::THEME_PADDING)
        .into()
    }
}
