use iced::widget::{button, column, container, row, text};
use iced::{Element, Fill, Task};
use plume_core::remote_probe::{self, Protocol, ReportedIdentity, ServerInfo};
use plume_core::remote_servers::AnisetteServer;
use rust_i18n::t;

use super::{Message, ProbeTarget};
use crate::{appearance, subscriptions::run_on_tokio};

#[derive(Debug, Default)]
pub(super) struct ServerInfoPanel {
    request: u64,
    address: Option<String>,
    checking: bool,
    result: Option<Result<ServerInfo, String>>,
}

impl ServerInfoPanel {
    pub fn clear(&mut self) {
        self.request += 1;
        self.address = None;
        self.checking = false;
        self.result = None;
    }

    pub fn probe(&mut self, server: &AnisetteServer, target: ProbeTarget) -> Task<Message> {
        if self.checking && self.address.as_ref() == Some(&server.address) {
            return Task::none();
        }
        self.clear();
        self.address = Some(server.address.clone());
        self.checking = true;
        let request = self.request;
        let address = server.address.clone();
        let path = crate::defaults::get_data_path();
        Task::perform(
            run_on_tokio(async move {
                remote_probe::probe_server(&address, &path)
                    .await
                    .map_err(|error| error.to_string())
            }),
            {
                let address = server.address.clone();
                move |result| Message::RemoteServerProbed(target, request, address.clone(), result)
            },
        )
    }

    pub fn probe_local(&mut self, config: plume_core::AnisetteConfiguration) -> Task<Message> {
        if self.checking {
            return Task::none();
        }
        self.clear();
        self.address = Some("local".into());
        self.checking = true;
        let request = self.request;
        Task::perform(
            run_on_tokio(async move {
                let start = std::time::Instant::now();
                plume_core::auth::anisette_data::AnisetteData::new(config)
                    .await
                    .map(|data| ServerInfo::from_local_headers(&data.base_headers, start.elapsed()))
                    .map_err(|error| error.to_string())
            }),
            move |result| {
                Message::RemoteServerProbed(ProbeTarget::Current, request, "local".into(), result)
            },
        )
    }

    pub fn probed(&mut self, request: u64, address: String, result: Result<ServerInfo, String>) {
        if self.request == request && self.address.as_ref() == Some(&address) {
            self.checking = false;
            self.result = Some(result);
        }
    }

    pub fn view<'a>(&'a self, server: &AnisetteServer, refresh: Message) -> Element<'a, Message> {
        let endpoint = reqwest::Url::parse(&server.address)
            .ok()
            .map(|url| url.origin().ascii_serialization())
            .unwrap_or_default();
        let mut content = column![
            row![
                text(t!("server_info_title")).width(Fill),
                button(text(t!("server_info_probe")))
                    .on_press_maybe((!self.checking).then_some(refresh))
                    .style(appearance::s_button),
            ]
            .align_y(iced::Alignment::Center),
            text(if endpoint.is_empty() {
                server.name.clone()
            } else {
                format!("{} - {endpoint}", server.name)
            }),
        ]
        .spacing(appearance::THEME_PADDING);
        if self.checking {
            content = content.push(text(t!("server_info_checking")));
        } else if let Some(result) = &self.result {
            match result {
                Ok(info) => {
                    let protocol = match info.protocol {
                        Protocol::Local => "local",
                        Protocol::V1 => "v1",
                        Protocol::V3 => "v3",
                    };
                    content = content.push(text(t!(
                        "server_info_response",
                        protocol = protocol,
                        ms = info.elapsed.as_millis().to_string()
                    )));
                    let identity = match info.identity {
                        ReportedIdentity::Mac => t!("server_identity_mac"),
                        ReportedIdentity::Windows => t!("server_identity_windows"),
                        ReportedIdentity::Linux => t!("server_identity_linux"),
                        ReportedIdentity::AppleMobile => t!("server_identity_ios"),
                        ReportedIdentity::Unknown => t!("server_identity_unknown"),
                    };
                    content = content.push(text(t!("server_info_identity", identity = identity)));
                    for (label, value) in [
                        (t!("server_info_model"), info.model.as_deref()),
                        (t!("server_info_os"), info.operating_system.as_deref()),
                        (t!("server_info_software"), info.server_software.as_deref()),
                        (t!("server_info_client"), info.client_info.as_deref()),
                        (t!("server_info_agent"), info.user_agent.as_deref()),
                    ] {
                        if let Some(value) = value {
                            content = content.push(
                                text(format!("{label}: {value}"))
                                    .size(appearance::THEME_FONT_SIZE - 1.0),
                            );
                        }
                    }
                    if info.protocol != Protocol::Local {
                        content = content.push(
                            text(t!("server_info_reported_note"))
                                .size(appearance::THEME_FONT_SIZE - 1.0),
                        );
                    }
                }
                Err(error) => {
                    content = content
                        .push(text(t!("server_info_failed")))
                        .push(text(error).size(appearance::THEME_FONT_SIZE - 1.0));
                }
            }
        } else {
            content = content.push(text(t!("server_info_unchecked")));
        }
        container(content.width(Fill))
            .padding(appearance::THEME_PADDING)
            .style(container::rounded_box)
            .width(Fill)
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_server_results_do_not_overwrite_the_selected_server_or_reset_state() {
        let mut panel = ServerInfoPanel {
            request: 2,
            address: Some("https://new.example".into()),
            checking: true,
            result: None,
        };
        panel.probed(1, "https://old.example".into(), Err("old failure".into()));
        assert!(panel.checking);
        assert!(panel.result.is_none());
        panel.clear();
        panel.probed(2, "https://new.example".into(), Err("late failure".into()));
        assert!(panel.result.is_none());
    }
}
