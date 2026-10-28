use iced::widget::{button, column, container, row, rule, scrollable, text};
use iced::{Alignment, Element, Fill, Task};
use plume_store::GsaAccount;
use rust_i18n::t;

use crate::appearance;
use crate::subscriptions::{confirm, developer_session, run_on_tokio};

#[derive(Debug, Clone)]
pub enum Message {
    Load,
    Loaded(Result<Overview, String>),
    RevokeCertificate(Certificate),
    DeleteAppId(AppId),
    SetDeviceEnabled(plume_utils::RegisteredDevice, bool),
    ActionDone(Result<bool, String>),
}

#[derive(Debug, Clone)]
pub struct Certificate {
    pub name: String,
    pub machine_name: Option<String>,
    pub serial_number: String,
    pub expires: String,
}

#[derive(Debug, Clone)]
pub struct AppId {
    pub id: String,
    pub name: String,
    pub identifier: String,
}

#[derive(Debug, Clone)]
pub struct Overview {
    team_id: String,
    certificates: Vec<Certificate>,
    app_ids: Vec<AppId>,
    devices: Result<Vec<plume_utils::RegisteredDevice>, String>,
}

#[derive(Debug, Default)]
pub struct DeveloperTab {
    overview: Option<Overview>,
    loading: bool,
    error: Option<String>,
}

async fn load(account: GsaAccount) -> Result<Overview, String> {
    let (session, team_id) = developer_session(&account).await?;
    let certificates = session
        .qh_list_certs(&team_id)
        .await
        .map_err(|e| e.to_string())?
        .certificates
        .into_iter()
        .map(|c| Certificate {
            name: c.name,
            machine_name: c.machine_name,
            serial_number: c.serial_number,
            expires: c.expiration_date.to_xml_format().chars().take(10).collect(),
        })
        .collect();
    let app_ids = session
        .qh_list_app_ids(&team_id)
        .await
        .map_err(|e| e.to_string())?
        .app_ids
        .into_iter()
        .map(|a| AppId {
            id: a.app_id_id,
            name: a.name,
            identifier: a.identifier,
        })
        .collect();
    let devices = match device_manager(&account, &team_id).await {
        Ok(manager) => manager
            .registered_devices()
            .await
            .map_err(|error| error.to_string()),
        Err(error) => Err(error),
    };
    Ok(Overview {
        team_id,
        certificates,
        app_ids,
        devices,
    })
}

async fn device_manager(
    account: &GsaAccount,
    team_id: &str,
) -> Result<plume_utils::XkitSigner, String> {
    plume_utils::XkitSigner::new(
        account.adsid(),
        account.xcode_gs_token(),
        plume_store::AnisetteSettings::load_configuration(&crate::defaults::get_data_path()),
        team_id,
        &crate::defaults::get_data_path(),
    )
    .await
    .map_err(|error| error.to_string())
}

impl DeveloperTab {
    pub fn update(&mut self, message: Message, account: Option<&GsaAccount>) -> Task<Message> {
        match message {
            Message::Load => {
                let Some(account) = account.cloned() else {
                    self.overview = None;
                    return Task::none();
                };
                if self.loading {
                    return Task::none();
                }
                self.loading = true;
                self.error = None;
                Task::perform(run_on_tokio(load(account)), Message::Loaded)
            }
            Message::Loaded(result) => {
                self.loading = false;
                match result {
                    Ok(overview) => self.overview = Some(overview),
                    Err(e) => {
                        log::error!("Failed to load developer account details: {e}");
                        self.error = Some(e);
                    }
                }
                Task::none()
            }
            Message::RevokeCertificate(cert) => {
                let Some(account) = account.cloned() else {
                    return Task::none();
                };
                self.loading = true;
                Task::perform(
                    run_on_tokio(async move {
                        if !confirm(
                            t!("developer_revoke").to_string(),
                            t!("developer_revoke_confirm", name = cert.name.clone()).to_string(),
                        )
                        .await
                        {
                            return Ok(false);
                        }
                        let (session, team_id) = developer_session(&account).await?;
                        session
                            .qh_revoke_cert(&team_id, &cert.serial_number)
                            .await
                            .map_err(|e| e.to_string())?;
                        log::info!("Revoked certificate {}", cert.serial_number);
                        Ok(true)
                    }),
                    Message::ActionDone,
                )
            }
            Message::DeleteAppId(app_id) => {
                let Some(account) = account.cloned() else {
                    return Task::none();
                };
                self.loading = true;
                Task::perform(
                    run_on_tokio(async move {
                        if !confirm(
                            t!("developer_delete_app_id").to_string(),
                            t!(
                                "developer_delete_app_id_confirm",
                                identifier = app_id.identifier.clone()
                            )
                            .to_string(),
                        )
                        .await
                        {
                            return Ok(false);
                        }
                        let (session, team_id) = developer_session(&account).await?;
                        session
                            .qh_delete_app_id(&team_id, &app_id.id)
                            .await
                            .map_err(|e| e.to_string())?;
                        log::info!("Deleted app ID {}", app_id.identifier);
                        Ok(true)
                    }),
                    Message::ActionDone,
                )
            }
            Message::SetDeviceEnabled(device, enabled) => {
                let Some(account) = account.cloned() else {
                    return Task::none();
                };
                if self.loading {
                    return Task::none();
                }
                self.loading = true;
                Task::perform(
                    run_on_tokio(async move {
                        let action = if enabled { "Enable" } else { "Disable" };
                        if !confirm(format!("{action} registered device"), format!("{action} {} ({})? Disabling invalidates profiles that include this device and does not free an annual device slot. Apple requires Account Holder or Admin access.", device.name, device.udid)).await { return Ok(false); }
                        let (_, team_id) = developer_session(&account).await?;
                        device_manager(&account, &team_id)
                            .await?
                            .set_device_enabled(&device.id, enabled)
                            .await
                            .map_err(|error| {
                                format!("Apple could not {action} this device: {error}")
                            })?;
                        Ok(true)
                    }),
                    Message::ActionDone,
                )
            }
            Message::ActionDone(result) => {
                self.loading = false;
                match result {
                    Ok(true) => return self.update(Message::Load, account),
                    Ok(false) => {}
                    Err(e) => {
                        log::error!("Developer account action failed: {e}");
                        self.error = Some(e);
                    }
                }
                Task::none()
            }
        }
    }

    pub fn view<'a>(&'a self, account: Option<&'a GsaAccount>) -> Element<'a, Message> {
        let header = match account {
            Some(account) => {
                let team = self
                    .overview
                    .as_ref()
                    .map(|o| o.team_id.as_str())
                    .unwrap_or(account.team_id());
                t!("developer_account", email = account.email(), team = team)
            }
            None => t!("developer_no_account"),
        };
        let refresh_label = if self.loading {
            t!("utilities_loading")
        } else {
            t!("refresh")
        };

        let mut content = column![
            row![
                text(header).width(Fill),
                button(text(refresh_label))
                    .on_press_maybe((account.is_some() && !self.loading).then_some(Message::Load))
                    .style(appearance::s_button),
            ]
            .spacing(appearance::THEME_PADDING)
            .align_y(Alignment::Center)
        ]
        .spacing(appearance::THEME_PADDING);

        if let Some(error) = &self.error {
            content = content.push(
                text(error)
                    .size(appearance::THEME_FONT_SIZE)
                    .color(iced::Color::from_rgb(0.9, 0.2, 0.2)),
            );
        }

        let Some(overview) = &self.overview else {
            return scrollable(content).height(Fill).into();
        };
        let enabled = !self.loading;
        let small = appearance::THEME_FONT_SIZE - 1.0;

        content = content.push(text(t!(
            "developer_certificates",
            count = overview.certificates.len()
        )));
        for cert in &overview.certificates {
            let machine = cert.machine_name.as_deref().unwrap_or("?");
            content = content.push(
                row![
                    column![
                        text(format!("{} – {machine}", cert.name)),
                        text(t!(
                            "developer_certificate_details",
                            serial = cert.serial_number.as_str(),
                            expires = cert.expires.as_str()
                        ))
                        .size(small),
                    ]
                    .width(Fill),
                    button(text(t!("developer_revoke")))
                        .on_press_maybe(enabled.then(|| Message::RevokeCertificate(cert.clone())))
                        .style(appearance::s_button),
                ]
                .spacing(appearance::THEME_PADDING)
                .align_y(Alignment::Center),
            );
        }

        content = content.push(container(rule::horizontal(1)).padding([appearance::THEME_PADDING / 2.0, 0.0]))
            .push(text("Registered devices"))
            .push(text("Devices can be enabled or disabled, not permanently deleted. Disabling invalidates affected profiles and does not free an annual slot. Requires Account Holder or Admin access.").size(small));
        match &overview.devices {
            Ok(devices) => {
                for device in devices {
                    let target_enabled = device.status == "DISABLED";
                    let known_status = matches!(device.status.as_str(), "ENABLED" | "DISABLED");
                    content = content.push(
                        row![
                            column![
                                text(&device.name),
                                text(format!(
                                    "{} · {} · {}",
                                    device.udid, device.platform, device.status
                                ))
                                .size(small)
                            ]
                            .width(Fill),
                            button(text(if target_enabled { "Enable" } else { "Disable" }))
                                .on_press_maybe((enabled && known_status).then(|| {
                                    Message::SetDeviceEnabled(device.clone(), target_enabled)
                                }))
                                .style(appearance::s_button),
                        ]
                        .spacing(appearance::THEME_PADDING)
                        .align_y(Alignment::Center),
                    );
                }
            }
            Err(error) => {
                content = content.push(
                    text(format!(
                        "Device management unavailable for this account: {error}"
                    ))
                    .size(small),
                );
            }
        }

        content = content
            .push(container(rule::horizontal(1)).padding([appearance::THEME_PADDING / 2.0, 0.0]))
            .push(text(t!(
                "developer_app_ids",
                count = overview.app_ids.len()
            )));
        for app_id in &overview.app_ids {
            content = content.push(
                row![
                    column![text(&app_id.name), text(&app_id.identifier).size(small)].width(Fill),
                    button(text(t!("pairing_delete")))
                        .on_press_maybe(enabled.then(|| Message::DeleteAppId(app_id.clone())))
                        .style(appearance::s_button),
                ]
                .spacing(appearance::THEME_PADDING)
                .align_y(Alignment::Center),
            );
        }

        scrollable(content).height(Fill).into()
    }
}
