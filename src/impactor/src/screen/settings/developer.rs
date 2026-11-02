use iced::widget::{
    button, column, container, progress_bar, row, rule, scrollable, text, text_input,
};
use iced::{Alignment, Element, Fill, Task};
use plume_store::{AscKey, GsaAccount};
use plume_utils::{
    AccountKind, AccountLimits, DeveloperItem, Device, FREE_ACTIVE_APP_LIMIT, Meter, XkitSigner,
};
use rust_i18n::t;

use crate::appearance;
use crate::subscriptions::{confirm, developer_session, run_on_tokio};

#[derive(Debug, Clone)]
pub enum Message {
    Load,
    Loaded(
        Option<Result<Box<Overview>, String>>,
        Option<Result<Box<KeyOverview>, String>>,
    ),
    UpdateKeyId(String),
    UpdateIssuerId(String),
    PickKeyFile,
    KeyFilePicked(Option<Result<String, String>>),
    SaveKey,
    RemoveKey,
    /// Handled by the app, which stores the key and reloads this tab.
    KeyChanged(Option<AscKey>),
    DeleteItem(Credential, ItemKind, DeveloperItem),
    RegisterDevice(Credential),
    RevokeCertificate(Certificate),
    DeleteAppId(AppId),
    SetDeviceEnabled(plume_utils::RegisteredDevice, bool),
    ActionDone(Result<bool, String>),
}

/// Which login an action goes through when both an Apple ID and an API key are set up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Credential {
    AppleId,
    Key,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Certificate,
    BundleId,
    Profile,
}

/// What an App Store Connect key can see of its team.
#[derive(Debug, Clone)]
pub struct KeyOverview {
    certificates: Vec<DeveloperItem>,
    bundle_ids: Vec<DeveloperItem>,
    profiles: Vec<DeveloperItem>,
    devices: Vec<plume_utils::RegisteredDevice>,
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
    profiles: Result<Vec<DeveloperItem>, String>,
    /// Name of the selected device and its active sideloaded apps.
    active_apps: Option<(String, Meter)>,
}

#[derive(Debug, Default)]
pub struct DeveloperTab {
    overview: Option<Overview>,
    key_overview: Option<KeyOverview>,
    loading: bool,
    error: Option<String>,
    draft_key_id: String,
    draft_issuer_id: String,
    draft_pem: String,
}

async fn load_key(key: AscKey) -> Result<KeyOverview, String> {
    let signer = XkitSigner::with_asc_key(&key, &crate::defaults::get_data_path())
        .map_err(|error| error.to_string())?;
    let text = |error: plume_utils::Error| error.to_string();
    Ok(KeyOverview {
        certificates: signer.certificates().await.map_err(text)?,
        bundle_ids: signer.bundle_ids().await.map_err(text)?,
        profiles: signer.profiles().await.map_err(text)?,
        devices: signer.registered_devices().await.map_err(text)?,
    })
}

/// The xkit client for `credential`, which must be available.
async fn manager_for(
    credential: Credential,
    account: Option<GsaAccount>,
    key: Option<AscKey>,
) -> Result<XkitSigner, String> {
    match credential {
        Credential::Key => {
            let key = key.ok_or("No App Store Connect key is saved")?;
            XkitSigner::with_asc_key(&key, &crate::defaults::get_data_path())
                .map_err(|error| error.to_string())
        }
        Credential::AppleId => {
            let account = account.ok_or("No account is selected")?;
            let (_, team_id) = developer_session(&account).await?;
            device_manager(&account, &team_id).await
        }
    }
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
    let profiles = match device_manager(&account, &team_id).await {
        Ok(manager) => manager.profiles().await.map_err(|error| error.to_string()),
        Err(error) => Err(error),
    };
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
        profiles,
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
        key: Option<&AscKey>,
        device: Option<&Device>,
    ) -> Task<Message> {
        match message {
            Message::Load => {
                if account.is_none() {
                    self.overview = None;
                }
                if key.is_none() {
                    self.key_overview = None;
                }
                if self.loading || (account.is_none() && key.is_none()) {
                    return Task::none();
                }
                self.loading = true;
                self.error = None;
                let (account, key, device) = (account.cloned(), key.cloned(), device.cloned());
                Task::perform(
                    run_on_tokio(async move {
                        let overview = match account {
                            Some(account) => Some(load(account, device).await.map(Box::new)),
                            None => None,
                        };
                        let key_overview = match key {
                            Some(key) => Some(load_key(key).await.map(Box::new)),
                            None => None,
                        };
                        (overview, key_overview)
                    }),
                    |(overview, key_overview)| Message::Loaded(overview, key_overview),
                )
            }
            Message::Loaded(overview, key_overview) => {
                self.loading = false;
                for error in [
                    overview.as_ref().and_then(|r| r.as_ref().err()),
                    key_overview.as_ref().and_then(|r| r.as_ref().err()),
                ]
                .into_iter()
                .flatten()
                {
                    log::error!("Failed to load developer account details: {error}");
                    self.error = Some(error.clone());
                }
                if let Some(Ok(overview)) = overview {
                    self.overview = Some(*overview);
                }
                if let Some(Ok(key_overview)) = key_overview {
                    self.key_overview = Some(*key_overview);
                }
                Task::none()
            }
            Message::UpdateKeyId(value) => {
                self.draft_key_id = value;
                Task::none()
            }
            Message::UpdateIssuerId(value) => {
                self.draft_issuer_id = value;
                Task::none()
            }
            Message::PickKeyFile => Task::perform(
                async {
                    let file = rfd::AsyncFileDialog::new()
                        .add_filter("App Store Connect key", &["p8"])
                        .set_title("Select .p8 key")
                        .pick_file()
                        .await?;
                    Some(
                        std::fs::read_to_string(file.path())
                            .map_err(|e| format!("Could not read the key: {e}")),
                    )
                },
                Message::KeyFilePicked,
            ),
            Message::KeyFilePicked(result) => {
                match result {
                    Some(Ok(pem)) => {
                        self.draft_pem = pem;
                        self.error = None;
                    }
                    Some(Err(error)) => self.error = Some(error),
                    None => {}
                }
                Task::none()
            }
            Message::SaveKey => {
                let key = AscKey {
                    key_id: self.draft_key_id.trim().to_owned(),
                    issuer_id: self.draft_issuer_id.trim().to_owned(),
                    pem: self.draft_pem.clone(),
                };
                if let Err(error) =
                    XkitSigner::with_asc_key(&key, &crate::defaults::get_data_path())
                {
                    self.error = Some(error.to_string());
                    return Task::none();
                }
                self.error = None;
                self.draft_pem.clear();
                Task::done(Message::KeyChanged(Some(key)))
            }
            Message::RemoveKey => Task::done(Message::KeyChanged(None)),
            Message::KeyChanged(_) => Task::none(),
            Message::DeleteItem(credential, kind, item) => {
                let (account, key) = (account.cloned(), key.cloned());
                self.loading = true;
                Task::perform(
                    run_on_tokio(async move {
                        if !confirm(
                            t!("pairing_delete").to_string(),
                            t!("developer_delete_item_confirm", name = item.name.clone())
                                .to_string(),
                        )
                        .await
                        {
                            return Ok(false);
                        }
                        let manager = manager_for(credential, account, key).await?;
                        match kind {
                            ItemKind::Certificate => manager.revoke_certificate(&item.id).await,
                            ItemKind::BundleId => manager.delete_bundle_id(&item.id).await,
                            ItemKind::Profile => manager.delete_profile(&item.id).await,
                        }
                        .map_err(|error| error.to_string())?;
                        log::info!("Deleted {kind:?} {}", item.id);
                        Ok(true)
                    }),
                    Message::ActionDone,
                )
            }
            Message::RegisterDevice(credential) => {
                let Some(device) = device.cloned() else {
                    return Task::none();
                };
                let (account, key) = (account.cloned(), key.cloned());
                self.loading = true;
                Task::perform(
                    run_on_tokio(async move {
                        manager_for(credential, account, key)
                            .await?
                            .register_device(&device.udid, &device.name)
                            .await
                            .map_err(|error| error.to_string())?;
                        log::info!("Registered {}", device.udid);
                        Ok(true)
                    }),
                    Message::ActionDone,
                )
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
                    Ok(true) => return self.update(Message::Load, account, key, device),
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

    fn view_item<'a>(
        credential: Credential,
        kind: ItemKind,
        item: &'a DeveloperItem,
        enabled: bool,
    ) -> Element<'a, Message> {
        row![
            column![
                text(&item.name),
                text(&item.detail).size(appearance::THEME_FONT_SIZE - 1.0)
            ]
            .width(Fill),
            button(text(t!("pairing_delete")))
                .on_press_maybe(enabled.then(|| Message::DeleteItem(
                    credential,
                    kind,
                    item.clone()
                )))
                .style(appearance::s_button),
        ]
        .spacing(appearance::THEME_PADDING)
        .align_y(Alignment::Center)
        .into()
    }

    fn view_key(&self, key: Option<&AscKey>, device: Option<&Device>) -> Element<'_, Message> {
        let small = appearance::THEME_FONT_SIZE - 1.0;
        let enabled = !self.loading;
        let mut content = column![
            text(t!("developer_key_title")).size(appearance::THEME_FONT_SIZE + 1.0),
            text(t!("developer_key_desc")).size(small),
        ]
        .spacing(appearance::THEME_PADDING);

        let Some(key) = key else {
            let ready = !self.draft_key_id.trim().is_empty()
                && !self.draft_issuer_id.trim().is_empty()
                && !self.draft_pem.is_empty();
            let file = if self.draft_pem.is_empty() {
                t!("developer_key_pick")
            } else {
                t!("developer_key_picked")
            };
            return content
                .push(
                    text_input(&t!("developer_key_id"), &self.draft_key_id)
                        .on_input(Message::UpdateKeyId)
                        .padding(6),
                )
                .push(
                    text_input(&t!("developer_key_issuer"), &self.draft_issuer_id)
                        .on_input(Message::UpdateIssuerId)
                        .padding(6),
                )
                .push(
                    row![
                        button(text(file))
                            .on_press(Message::PickKeyFile)
                            .style(appearance::s_button),
                        button(text(t!("developer_key_save")))
                            .on_press_maybe(ready.then_some(Message::SaveKey))
                            .style(appearance::p_button),
                    ]
                    .spacing(appearance::THEME_PADDING),
                )
                .into();
        };

        let mut actions =
            row![text(t!("developer_key_saved", id = key.key_id.as_str())).width(Fill),]
                .spacing(appearance::THEME_PADDING)
                .align_y(Alignment::Center);
        if let Some(device) = device.filter(|device| !device.is_mac) {
            actions = actions.push(
                button(text(t!(
                    "developer_register",
                    device = device.name.as_str()
                )))
                .on_press_maybe(enabled.then_some(Message::RegisterDevice(Credential::Key)))
                .style(appearance::s_button),
            );
        }
        actions = actions.push(
            button(text(t!("developer_key_remove")))
                .on_press(Message::RemoveKey)
                .style(appearance::s_button),
        );
        content = content.push(actions);

        let Some(overview) = &self.key_overview else {
            return content.into();
        };
        for (title, kind, items) in [
            (
                t!(
                    "developer_certificates",
                    count = overview.certificates.len()
                ),
                ItemKind::Certificate,
                &overview.certificates,
            ),
            (
                t!("developer_bundle_ids", count = overview.bundle_ids.len()),
                ItemKind::BundleId,
                &overview.bundle_ids,
            ),
            (
                t!("developer_profiles", count = overview.profiles.len()),
                ItemKind::Profile,
                &overview.profiles,
            ),
        ] {
            content = content.push(text(title.into_owned()));
            if items.is_empty() {
                content = content.push(text(t!("developer_none")).size(small));
            }
            for item in items {
                content = content.push(Self::view_item(Credential::Key, kind, item, enabled));
            }
        }
        content = content.push(text(
            t!("developer_devices", count = overview.devices.len()).into_owned(),
        ));
        for device in &overview.devices {
            content = content.push(
                text(format!(
                    "{} · {} · {}",
                    device.name, device.status, device.udid
                ))
                .size(small),
            );
        }
        content.into()
    }

    pub fn view<'a>(
        &'a self,
        account: Option<&'a GsaAccount>,
        key: Option<&'a AscKey>,
        device: Option<&'a Device>,
    ) -> Element<'a, Message> {
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
            return scrollable(content.push(self.view_key(key, device)))
                .height(Fill)
                .into();
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

        content = content.push(divider());
        match &overview.profiles {
            Ok(profiles) => {
                content = content.push(section(t!("developer_profiles", count = profiles.len())));
                if profiles.is_empty() {
                    content = content.push(text(t!("developer_none")).size(small));
                }
                for profile in profiles {
                    content = content.push(Self::view_item(
                        Credential::AppleId,
                        ItemKind::Profile,
                        profile,
                        enabled,
                    ));
                }
            }
            Err(error) => {
                content = content.push(
                    text(t!("developer_profiles_unavailable", error = error.as_str())).size(small),
                );
            }
        }
        if let Some(device) = device.filter(|device| !device.is_mac) {
            content = content.push(
                button(text(t!(
                    "developer_register",
                    device = device.name.as_str()
                )))
                .on_press_maybe(enabled.then_some(Message::RegisterDevice(Credential::AppleId)))
                .style(appearance::s_button),
            );
        }

        content = content.push(divider()).push(self.view_key(key, device));
        scrollable(content).height(Fill).into()
    }
}
