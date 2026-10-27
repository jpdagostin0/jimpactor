use iced::widget::{button, column, row, scrollable, text, text_input};
use iced::{Alignment, Element, Fill, Task};
use rust_i18n::t;

use crate::appearance;
use crate::defaults::get_data_path;
use crate::sources::{self, Source, SourcesStore};
use crate::subscriptions::run_on_tokio;

#[derive(Debug, Clone)]
pub enum Message {
    UpdateUrl(String),
    Add,
    Identified(Result<Source, String>),
    Remove(String),
}

impl Message {
    /// Whether handling this message can change the saved sources.
    pub fn changes_sources(&self) -> bool {
        matches!(self, Message::Identified(Ok(_)) | Message::Remove(_))
    }
}

#[derive(Debug)]
pub struct SourcesTab {
    store: SourcesStore,
    url: String,
    adding: bool,
    status: Option<Result<String, String>>,
}

impl SourcesTab {
    pub fn new() -> Self {
        Self {
            store: SourcesStore::load(&get_data_path()),
            url: String::new(),
            adding: false,
            status: None,
        }
    }

    fn save(&mut self) {
        if let Err(e) = self.store.save(&get_data_path()) {
            log::error!("Failed to save sources: {e}");
            self.status = Some(Err(e));
        }
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::UpdateUrl(url) => {
                self.url = url;
                Task::none()
            }
            Message::Add => {
                let url = self.url.trim().to_string();
                if url.is_empty() || self.adding {
                    return Task::none();
                }
                self.adding = true;
                self.status = Some(Ok(t!("sources_identifying").to_string()));
                Task::perform(
                    run_on_tokio(async move { sources::identify(&url).await }),
                    Message::Identified,
                )
            }
            Message::Identified(result) => {
                self.adding = false;
                match result.and_then(|source| {
                    let added = t!(
                        "sources_added",
                        name = source.name.clone(),
                        kind = source.kind.to_string()
                    )
                    .to_string();
                    self.store.add(source).map(|_| added)
                }) {
                    Ok(added) => {
                        log::info!("{added}");
                        self.url.clear();
                        self.status = Some(Ok(added));
                        self.save();
                    }
                    Err(e) => {
                        log::warn!("Failed to add source: {e}");
                        self.status = Some(Err(e));
                    }
                }
                Task::none()
            }
            Message::Remove(url) => {
                self.store.remove(&url);
                self.status = None;
                self.save();
                Task::none()
            }
        }
    }

    pub fn view(&self) -> Element<'_, Message> {
        let input = text_input(&t!("sources_placeholder"), &self.url)
            .on_input(Message::UpdateUrl)
            .on_submit(Message::Add)
            .padding(6)
            .width(Fill);
        let add = button(appearance::icon_text(
            appearance::PLUS,
            t!("sources_add"),
            None,
        ))
        .on_press_maybe((!self.adding && !self.url.trim().is_empty()).then_some(Message::Add))
        .style(appearance::s_button);

        let mut content = column![
            text(t!("sources_desc")).size(appearance::THEME_FONT_SIZE - 1.0),
            row![input, add]
                .spacing(appearance::THEME_PADDING)
                .align_y(Alignment::Center),
        ]
        .spacing(appearance::THEME_PADDING);

        if let Some(status) = &self.status {
            let (msg, color) = match status {
                Ok(msg) => (msg, iced::Color::from_rgb(0.2, 0.8, 0.4)),
                Err(msg) => (msg, iced::Color::from_rgb(0.9, 0.2, 0.2)),
            };
            content = content.push(text(msg).size(appearance::THEME_FONT_SIZE).color(color));
        }

        for source in self.store.sources() {
            let remove = button(appearance::icon_text(
                appearance::MINUS,
                t!("sources_remove"),
                None,
            ))
            .on_press_maybe((!source.is_default()).then(|| Message::Remove(source.url.clone())))
            .style(appearance::s_button);
            content = content.push(
                row![
                    column![
                        text(format!("{} [{}]", source.name, source.kind)),
                        text(&source.url).size(appearance::THEME_FONT_SIZE - 1.0),
                    ]
                    .width(Fill),
                    remove,
                ]
                .spacing(appearance::THEME_PADDING)
                .align_y(Alignment::Center),
            );
        }

        scrollable(content).height(Fill).into()
    }
}
