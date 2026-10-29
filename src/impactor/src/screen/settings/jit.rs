use iced::widget::{button, checkbox, column, container, pick_list, row, scrollable, text, text_input};
use iced::{Alignment, Element, Fill, Task};
use plume_store::{JitListen, JitServerSettings};
use rust_i18n::t;

use crate::appearance;
use crate::jit_server::JitServerStatus;

#[derive(Debug, Clone)]
pub enum Message {
    ToggleEnabled(bool),
    SelectListen(ListenChoice),
    UpdatePort(String),
    Apply,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListenChoice(pub JitListen);

impl std::fmt::Display for ListenChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self.0 {
            JitListen::Local => t!("jit_listen_local"),
            JitListen::Network => t!("jit_listen_network"),
            JitListen::Tailnet => t!("jit_listen_tailnet"),
        };
        f.write_str(&name)
    }
}

#[derive(Debug)]
pub struct JitTab {
    draft: JitServerSettings,
    port: String,
    error: Option<String>,
}

impl JitTab {
    pub fn new(settings: JitServerSettings) -> Self {
        Self {
            port: settings.port.to_string(),
            draft: settings,
            error: None,
        }
    }

    /// Returns new settings for the app to save and apply.
    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<JitServerSettings>) {
        let save = match message {
            Message::ToggleEnabled(enabled) => {
                self.draft.enabled = enabled;
                true
            }
            Message::SelectListen(ListenChoice(listen)) => {
                self.draft.listen = listen;
                true
            }
            Message::UpdatePort(port) => {
                self.port = port;
                false
            }
            Message::Apply => true,
        };
        if !save {
            return (Task::none(), None);
        }
        match self.port.trim().parse::<u16>() {
            Ok(port) if port != 0 => {
                self.draft.port = port;
                self.error = None;
                (Task::none(), Some(self.draft.clone()))
            }
            _ => {
                self.error = Some(t!("jit_invalid_port").to_string());
                (Task::none(), None)
            }
        }
    }

    pub fn view(&self) -> Element<'_, Message> {
        let small = appearance::THEME_FONT_SIZE - 1.0;
        let mut content = column![
            text(t!("settings_tab_jit")),
            text(t!("jit_desc")).size(small),
            checkbox(self.draft.enabled)
                .label(t!("jit_enable"))
                .on_toggle(Message::ToggleEnabled),
            text(t!("jit_listen")),
            pick_list(
                JitListen::ALL.map(ListenChoice),
                Some(ListenChoice(self.draft.listen)),
                Message::SelectListen,
            )
            .style(appearance::s_pick_list),
            text(t!("jit_port")),
            row![
                text_input("9172", &self.port)
                    .on_input(Message::UpdatePort)
                    .on_submit(Message::Apply)
                    .padding(6)
                    .width(120),
                button(text(t!("jit_apply")))
                    .on_press(Message::Apply)
                    .style(appearance::s_button),
            ]
            .spacing(appearance::THEME_PADDING)
            .align_y(Alignment::Center),
        ]
        .spacing(appearance::THEME_PADDING);

        if let Some(error) = &self.error {
            content = content.push(text(error).size(small));
        }

        let status = match crate::jit_server::status() {
            JitServerStatus::Off => t!("jit_status_off").to_string(),
            JitServerStatus::Starting => t!("jit_status_starting").to_string(),
            JitServerStatus::Listening(address) => {
                t!("jit_status_listening", address = address).to_string()
            }
            JitServerStatus::Failed(error) => t!("jit_status_failed", error = error).to_string(),
        };
        let mut info = column![text(status)].spacing(appearance::THEME_PADDING / 2.0);
        if self.draft.enabled {
            let base = format!("http://{}:{}", self.example_host(), self.draft.port);
            info = info
                .push(text(t!("jit_usage")).size(small))
                .push(text(format!("{base}/")).size(small))
                .push(text(format!("{base}/<UDID>")).size(small))
                .push(text(format!("{base}/<UDID>/<bundle id>")).size(small))
                .push(text(format!("{base}/<UDID>/attach/<pid>")).size(small));
            if self.draft.listen != JitListen::Local {
                info = info.push(text(t!("jit_security_note")).size(small));
            }
        }
        content = content.push(
            container(info)
                .padding(appearance::THEME_PADDING)
                .style(container::rounded_box)
                .width(Fill),
        );

        scrollable(content.padding([0.0, appearance::THEME_PADDING]).width(Fill))
            .height(Fill)
            .into()
    }

    fn example_host(&self) -> String {
        match self.draft.listen {
            JitListen::Local => "127.0.0.1".into(),
            JitListen::Network => lan_address().unwrap_or_else(|| "<this computer>".into()),
            JitListen::Tailnet => match plume_utils::tailnet_status() {
                plume_utils::TailnetStatus::Connected { address } => address.to_string(),
                _ => "<tailnet address>".into(),
            },
        }
    }
}

/// The address this computer uses on the local network. Connecting a UDP socket only
/// selects a route; nothing is sent.
fn lan_address() -> Option<String> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:80").ok()?;
    Some(socket.local_addr().ok()?.ip().to_string())
}
