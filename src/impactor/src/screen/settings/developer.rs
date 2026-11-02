use iced::widget::{button, column, container, progress_bar, row, rule, scrollable, text};
use iced::{Alignment, Element, Fill, Task};
use plume_store::GsaAccount;
use plume_utils::{AccountKind, AccountLimits, Device, FREE_ACTIVE_APP_LIMIT, Meter};
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
    limits: Result<AccountLimits, String>,
    /// Name of the selected device and its active sideloaded apps.
    active_apps: Option<(String, Meter)>,
}

#[derive(Debug, Default)]
pub struct DeveloperTab {
    overview: Option<Overview>,
    loading: bool,
    error: Option<String>,
}

async fn load(account: GsaAccount, device: Option<Device>) -> Result<Overview, String> {
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
    let limits = AccountLimits::fetch(&session, &team_id)
        .await
        .map_err(|error| error.to_string());
    let free = matches!(&limits, Ok(limits) if limits.kind == AccountKind::Free);
    let mut active_apps = None;
    if let Some(device) = device.filter(|device| !device.is_mac && device.is_available()) {
        match device.active_sideloaded_apps().await {
            Ok(used) => {
                let limit = free.then_some(FREE_ACTIVE_APP_LIMIT);
                active_apps = Some((device.name.clone(), Meter { used, limit }));
            }
            Err(error) => log::debug!("Could not count apps on {}: {error}", device.name),
        }
    }
    Ok(Overview {
        team_id,
        certificates,
        app_ids,
        devices,
        limits,
        active_apps,
    })
}

fn view_meter<'a>(label: String, meter: Meter) -> Element<'a, Message> {
    let small = appearance::THEME_FONT_SIZE - 1.0;
    let mut title = text(format!("{label}: {meter}")).size(small);
    if meter.is_full() {
        title = title.color(iced::Color::from_rgb(0.9, 0.35, 0.35));
    }
    let mut column = column![title].spacing(2);
    if let Some(limit) = meter.limit {
        column = column
            .push(progress_bar(0.0..=limit.max(1) as f32, meter.used.min(limit) as f32).girth(6));
    }
    column.into()
}

fn view_limits(overview: &Overview) -> Element<'_, Message> {
    let small = appearance::THEME_FONT_SIZE - 1.0;
    let limits = match &overview.limits {
        Ok(limits) => limits,
        Err(error) => {
            return text(t!("developer_limits_unavailable", error = error.as_str()))
                .size(small)
                .into();
        }
    };
    let kind = match limits.kind {
        AccountKind::Free => t!("developer_kind_free"),
        AccountKind::Paid => t!("developer_kind_paid"),
    };
    let mut meters = column![
        text(format!("{kind} · {}", limits.team_name)),
        view_meter(t!("developer_meter_app_ids").into_owned(), limits.app_ids),
        view_meter(
            t!("developer_meter_devices").into_owned(),
            limits.registered_devices
        ),
    ]
    .spacing(appearance::THEME_PADDING / 2.0);
    if let Some((device, meter)) = &overview.active_apps {
        meters = meters.push(view_meter(
            t!("developer_meter_active_apps", device = device.as_str()).into_owned(),
            *meter,
        ));
    }
    meters =
        meters.push(text(t!("developer_profile_validity", days = limits.profile_days)).size(small));
    if limits.kind == AccountKind::Free {
        meters = meters.push(text(t!("developer_free_note")).size(small));
    }
    container(meters)
        .padding(appearance::THEME_PADDING)
        .style(container::rounded_box)
        .width(Fill)
        .into()
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
    pub fn update(
        &mut self,
        message: Message,
        account: Option<&GsaAccount>,
        device: Option<&Device>,
    ) -> Task<Message> {
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
                Task::perform(
                    run_on_tokio(load(account, device.cloned())),
                    Message::Loaded,
                )
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
                    Ok(true) => return self.update(Message::Load, account, device),
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
        content = content.push(view_limits(overview));

        let section = |title: std::borrow::Cow<'static, str>| {
            text(title.into_owned()).size(appearance::THEME_FONT_SIZE + 1.0)
        };
        let divider =
            || container(rule::horizontal(1)).padding([appearance::THEME_PADDING / 2.0, 0.0]);
        let item = |title: String, detail: String, action: Element<'a, Message>| {
            row![
                column![text(title), text(detail).size(small)].width(Fill),
                action,
            ]
            .spacing(appearance::THEME_PADDING)
            .align_y(Alignment::Center)
        };

        content = content.push(section(t!(
            "developer_certificates",
            count = overview.certificates.len()
        )));
        if overview.certificates.is_empty() {
            content = content.push(text(t!("developer_none")).size(small));
        }
        for cert in &overview.certificates {
            // Every certificate shares the account's name; the machine tells them apart.
            let title = cert.machine_name.as_deref().unwrap_or(cert.name.as_str());
            content = content.push(item(
                title.to_owned(),
                t!(
                    "developer_certificate_expires",
                    expires = cert.expires.as_str()
                )
                .into_owned(),
                button(text(t!("developer_revoke")))
                    .on_press_maybe(enabled.then(|| Message::RevokeCertificate(cert.clone())))
                    .style(appearance::s_button)
                    .into(),
            ));
        }

        content = content.push(divider());
        match &overview.devices {
            Ok(devices) => {
                content = content.push(section(t!("developer_devices", count = devices.len())));
                if devices.is_empty() {
                    content = content.push(text(t!("developer_none")).size(small));
                }
                for device in devices {
                    let target_enabled = device.status == "DISABLED";
                    let known_status = matches!(device.status.as_str(), "ENABLED" | "DISABLED");
                    let status = match device.status.as_str() {
                        "ENABLED" => t!("developer_device_enabled").into_owned(),
                        "DISABLED" => t!("developer_device_disabled").into_owned(),
                        other => other.to_owned(),
                    };
                    content = content.push(item(
                        device.name.clone(),
                        format!("{status} · {}", device.udid),
                        button(text(if target_enabled {
                            t!("developer_device_enable")
                        } else {
                            t!("developer_device_disable")
                        }))
                        .on_press_maybe(
                            (enabled && known_status)
                                .then(|| Message::SetDeviceEnabled(device.clone(), target_enabled)),
                        )
                        .style(appearance::s_button)
                        .into(),
                    ));
                }
            }
            Err(error) => {
                content = content.push(
                    text(t!("developer_devices_unavailable", error = error.as_str())).size(small),
                );
            }
        }

        content = content.push(divider()).push(section(t!(
            "developer_app_ids",
            count = overview.app_ids.len()
        )));
        if overview.app_ids.is_empty() {
            content = content.push(text(t!("developer_none")).size(small));
        }
        for app_id in &overview.app_ids {
            content = content.push(item(
                app_id.name.clone(),
                app_id.identifier.clone(),
                button(text(t!("pairing_delete")))
                    .on_press_maybe(enabled.then(|| Message::DeleteAppId(app_id.clone())))
                    .style(appearance::s_button)
                    .into(),
            ));
        }

        scrollable(content).height(Fill).into()
    }
}
