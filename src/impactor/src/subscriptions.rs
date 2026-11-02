use iced::{Subscription, window};
use idevice::usbmuxd::UsbmuxdAddr;
use std::sync::Arc;
#[cfg(target_os = "linux")]
use tray_icon::MouseButtonState;
use tray_icon::menu::MenuEvent;
use tray_icon::{MouseButton, TrayIconEvent};

use crate::{
    defaults::get_data_path,
    screen::{Message, general},
};
use plume_utils::{
    Device, DeviceEvent, watch_devices, watch_network_devices, watch_tailnet_devices,
};

pub(crate) fn device_listener() -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(
            100,
            |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
                use iced::futures::SinkExt;
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<DeviceEvent>();

                let network_tx = tx.clone();
                let known_wifi_devices = get_data_path().join("wifi_devices.plist");
                std::thread::Builder::new()
                    .name("jimpactor-wifi-discovery".into())
                    .spawn(move || {
                        let rt = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .expect("Could not start Wi-Fi discovery runtime");
                        rt.block_on(watch_network_devices(network_tx, known_wifi_devices));
                    })
                    .expect("Could not start Wi-Fi discovery thread");

                let tailnet_tx = tx.clone();
                std::thread::Builder::new()
                    .name("jimpactor-tailnet-discovery".into())
                    .spawn(move || {
                        let rt = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .expect("Could not start Tailscale discovery runtime");
                        rt.block_on(watch_tailnet_devices(tailnet_tx));
                    })
                    .expect("Could not start Tailscale discovery thread");

                std::thread::Builder::new()
                    .name("jimpactor-usb-discovery".into())
                    .spawn(move || {
                        let rt = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .unwrap();

                        rt.block_on(async move {
                            #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
                            {
                                if let Some(mac_udid) = plume_gestalt::get_udid() {
                                    let _ = tx.send(DeviceEvent::Connected(Device {
                                        name: "This Mac".into(),
                                        udid: mac_udid,
                                        device_id: u32::MAX,
                                        usbmuxd_device: None,
                                        network: None,
                                        is_mac: true,
                                    }));
                                }
                            }

                            watch_devices(UsbmuxdAddr::from_env_var().unwrap_or_default(), tx)
                                .await;
                        });
                    })
                    .expect("Could not start USB discovery thread");

                while let Some(event) = rx.recv().await {
                    let message = match event {
                        DeviceEvent::Connected(device) => Message::DeviceConnected(device),
                        DeviceEvent::Disconnected(id) => Message::DeviceDisconnected(id),
                    };
                    if output.send(message).await.is_err() {
                        break;
                    }
                }
            },
        )
    })
}

pub(crate) fn tray_subscription() -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(
            100,
            |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
                use iced::futures::{SinkExt, StreamExt};
                let (tx, mut rx) = iced::futures::channel::mpsc::unbounded::<Message>();

                // Forward native callbacks directly instead of polling tray channels.
                let menu_tx = tx.clone();
                MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
                    let _ = menu_tx.unbounded_send(Message::TrayMenuSelected(event.id));
                }));
                let icon_tx = tx.clone();
                TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
                    match event {
                        TrayIconEvent::DoubleClick {
                            button: MouseButton::Left,
                            ..
                        } => {
                            let _ = icon_tx.unbounded_send(Message::TrayIconClicked);
                        }
                        // StatusNotifier hosts handle the context menu themselves.
                        // Left-click activation opens the main window on Linux.
                        #[cfg(target_os = "linux")]
                        TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        } => {
                            let _ = icon_tx.unbounded_send(Message::TrayIconClicked);
                        }
                        _ => {}
                    }
                }));

                #[cfg(target_os = "macos")]
                std::thread::spawn(move || {
                    loop {
                        if tx.unbounded_send(Message::MacOsActivationTick).is_err() {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(32));
                    }
                });

                while let Some(message) = rx.next().await {
                    let _ = output.send(message).await;
                }
            },
        )
    })
}

pub(crate) fn tray_menu_refresh_subscription() -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(
            10,
            |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
                use iced::futures::{SinkExt, StreamExt};
                let (tx, mut rx) = iced::futures::channel::mpsc::unbounded::<Message>();

                std::thread::spawn(move || {
                    loop {
                        std::thread::sleep(std::time::Duration::from_secs(30));
                        let _ = tx.unbounded_send(Message::UpdateTrayMenu);
                    }
                });

                while let Some(message) = rx.next().await {
                    let _ = output.send(message).await;
                }
            },
        )
    })
}

pub(crate) fn logs_tick_subscription() -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(
            1,
            |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
                use iced::futures::{SinkExt, StreamExt};
                let (tx, mut rx) = iced::futures::channel::mpsc::unbounded::<Message>();

                std::thread::spawn(move || {
                    loop {
                        std::thread::sleep(std::time::Duration::from_secs(1));
                        let msg = Message::SettingsScreen(crate::screen::settings::Message::Logs(
                            crate::screen::settings::LogsMessage::Tick,
                        ));
                        if tx.unbounded_send(msg).is_err() {
                            break;
                        }
                    }
                });

                while let Some(message) = rx.next().await {
                    let _ = output.send(message).await;
                }
            },
        )
    })
}

/// Redraws once a second so live status (Tailscale, JIT server) stays current.
pub(crate) fn status_tick_subscription() -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(
            1,
            |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
                use iced::futures::SinkExt;
                loop {
                    futures_timer(std::time::Duration::from_secs(1)).await;
                    if output
                        .send(Message::SettingsScreen(
                            crate::screen::settings::Message::Noop,
                        ))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            },
        )
    })
}

/// Sleeps without needing a Tokio runtime on the GUI executor.
async fn futures_timer(duration: std::time::Duration) {
    let (tx, rx) = futures::channel::oneshot::channel::<()>();
    std::thread::spawn(move || {
        std::thread::sleep(duration);
        let _ = tx.send(());
    });
    let _ = rx.await;
}

pub(crate) fn certificate_reset_subscription() -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(
            10,
            |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
                use iced::futures::{SinkExt, StreamExt};
                let (tx, mut rx) = iced::futures::channel::mpsc::unbounded::<Message>();

                std::thread::spawn(move || {
                    while let Some(request) = crate::certificate_reset::wait_for_request() {
                        let _ = tx.unbounded_send(Message::CertificateResetRequested(request));
                    }
                });

                while let Some(message) = rx.next().await {
                    let _ = output.send(message).await;
                }
            },
        )
    })
}

#[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
pub(crate) fn relaunch_subscription() -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(
            10,
            |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
                use iced::futures::{SinkExt, StreamExt};
                let (tx, mut rx) = iced::futures::channel::mpsc::unbounded::<Message>();

                if let Err(err) = crate::relaunch::start_listener({
                    let tx = tx.clone();
                    move |action| {
                        let _ = tx.unbounded_send(match action {
                            crate::relaunch::RelaunchAction::Show => Message::RelaunchRequested,
                            crate::relaunch::RelaunchAction::Restart => Message::RestartRequested,
                        });
                    }
                }) {
                    log::warn!("Failed to start relaunch listener: {err}");
                }

                while let Some(message) = rx.next().await {
                    let _ = output.send(message).await;
                }
            },
        )
    })
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
pub(crate) fn relaunch_subscription() -> Subscription<Message> {
    Subscription::none()
}

pub(crate) fn file_hover_subscription() -> Subscription<Message> {
    window::events().filter_map(|(_id, event)| match event {
        window::Event::FileHovered(_) => Some(Message::MainScreen(general::Message::FilesHovered)),
        window::Event::FilesHoveredLeft => {
            Some(Message::MainScreen(general::Message::FilesHoveredLeft))
        }
        window::Event::FileDropped(path) => {
            Some(Message::MainScreen(general::Message::FilesDropped(vec![
                path,
            ])))
        }
        _ => None,
    })
}

type InstallationProgressReceiver = Arc<std::sync::Mutex<std::sync::mpsc::Receiver<(String, i32)>>>;

pub(crate) fn installation_progress_listener(
    progress_rx: Option<InstallationProgressReceiver>,
) -> Subscription<(String, i32)> {
    match progress_rx {
        Some(rx) => {
            struct State {
                rx: InstallationProgressReceiver,
            }

            impl std::hash::Hash for State {
                fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
                    Arc::as_ptr(&self.rx).hash(state);
                }
            }

            let state = State { rx };
            Subscription::run_with(state, |state| {
                let rx = state.rx.clone();
                iced::stream::channel(
                    100,
                    move |mut output: iced::futures::channel::mpsc::Sender<(String, i32)>| async move {
                        use iced::futures::{SinkExt, StreamExt};

                        let (tx, mut rx_stream) =
                            iced::futures::channel::mpsc::unbounded::<(String, i32)>();

                        let rx_thread = rx.clone();
                        std::thread::spawn(move || {
                            loop {
                                let message = {
                                    if let Ok(guard) = rx_thread.lock() {
                                        guard.try_recv().ok()
                                    } else {
                                        None
                                    }
                                };

                                if let Some((status, progress)) = message {
                                    let _ = tx.unbounded_send((status, progress));
                                }

                                std::thread::sleep(std::time::Duration::from_millis(100));
                            }
                        });

                        while let Some(message) = rx_stream.next().await {
                            let _ = output.send(message).await;
                        }
                    },
                )
            })
        }
        None => Subscription::none(),
    }
}

/// Connects the shared installer to the progress screen and the GUI's dialogs.
pub(crate) struct GuiInstallHost {
    pub tx: std::sync::mpsc::Sender<(String, i32)>,
}

impl plume_utils::install::InstallHost for GuiInstallHost {
    fn status(&self, message: String, percent: i32) {
        let _ = self.tx.send((message, percent));
    }

    fn confirm_certificate_reset(&self) -> bool {
        crate::certificate_reset::confirm()
    }

    fn export_path(
        &self,
        suggested_name: String,
    ) -> futures::future::BoxFuture<'_, Option<std::path::PathBuf>> {
        Box::pin(async move {
            rfd::AsyncFileDialog::new()
                .set_title("Save Package As")
                .set_file_name(suggested_name)
                .save_file()
                .await
                .map(|file| file.path().to_owned())
        })
    }
}

pub(crate) async fn export_certificate(account: plume_store::GsaAccount) -> Result<(), String> {
    let (team_id, p12_data) = plume_utils::session::export_certificate(
        &account,
        &get_data_path(),
        std::sync::Arc::new(crate::certificate_reset::confirm),
    )
    .await?;

    let file = rfd::AsyncFileDialog::new()
        .set_title("Save Certificate As")
        .set_file_name(format!("{team_id}_certificate.p12"))
        .save_file()
        .await;

    if let Some(save_path) = file {
        tokio::fs::write(save_path.path(), p12_data)
            .await
            .map_err(|error| {
                format!(
                    "Could not save certificate to {}: {error}",
                    save_path.path().display()
                )
            })?;
    }

    Ok(())
}

pub(crate) async fn fetch_teams(
    account: &plume_store::GsaAccount,
) -> Result<Vec<crate::screen::settings::Team>, String> {
    Ok(plume_utils::session::list_teams(account, &get_data_path())
        .await?
        .into_iter()
        .map(|(name, id)| crate::screen::settings::Team { name, id })
        .collect())
}

/// Opens a developer session for `account` and resolves the team to act on.
pub(crate) async fn developer_session(
    account: &plume_store::GsaAccount,
) -> Result<(plume_core::developer::DeveloperSession, String), String> {
    plume_utils::session::open_session(account, &get_data_path()).await
}

/// Runs `future` on a dedicated tokio runtime, since reqwest needs one and iced's executor isn't.
pub(crate) fn run_on_tokio<T: Send + 'static>(
    future: impl std::future::Future<Output = T> + Send + 'static,
) -> impl std::future::Future<Output = T> {
    let (tx, rx) = futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _ = tx.send(rt.block_on(future));
    });
    async move { rx.await.expect("background task panicked") }
}

/// An update from [`work_stream`].
#[derive(Debug, Clone)]
pub(crate) enum Work<T> {
    Progress(String),
    Done(T),
}

/// Runs `work` on its own tokio runtime, relaying what it reports while it runs.
pub(crate) fn work_stream<T, F, Fut>(work: F) -> impl iced::futures::Stream<Item = Work<T>>
where
    T: Send + 'static,
    F: FnOnce(tokio::sync::mpsc::UnboundedSender<String>) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = T> + Send + 'static,
{
    iced::stream::channel(
        32,
        move |mut output: iced::futures::channel::mpsc::Sender<Work<T>>| async move {
            use iced::futures::SinkExt;

            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let finished = run_on_tokio(work(tx));
            while let Some(status) = rx.recv().await {
                // Files can go by faster than they can be drawn; show the latest.
                let mut latest = status;
                while let Ok(status) = rx.try_recv() {
                    latest = status;
                }
                let _ = output.send(Work::Progress(latest)).await;
            }
            let _ = output.send(Work::Done(finished.await)).await;
        },
    )
}

/// Asks a yes/no question in a native dialog.
pub(crate) async fn confirm(title: String, description: String) -> bool {
    rfd::AsyncMessageDialog::new()
        .set_level(rfd::MessageLevel::Warning)
        .set_title(title)
        .set_description(description)
        .set_buttons(rfd::MessageButtons::YesNo)
        .show()
        .await
        == rfd::MessageDialogResult::Yes
}

/// Streams the device syslog while the utilities screen asks for it. A new `session`
/// starts a fresh connection; dropping the subscription closes it.
pub(crate) fn syslog_subscription(
    device: Device,
    session: u64,
) -> Subscription<crate::screen::utilties::Message> {
    use crate::screen::utilties::Message as UtilitiesMessage;

    struct State {
        device: Device,
        session: u64,
    }

    impl std::hash::Hash for State {
        fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
            self.device.udid.hash(state);
            self.session.hash(state);
        }
    }

    Subscription::run_with(State { device, session }, |state| {
        let device = state.device.clone();
        iced::stream::channel(
            100,
            move |mut output: iced::futures::channel::mpsc::Sender<UtilitiesMessage>| async move {
                use iced::futures::SinkExt;
                // Lines can arrive by the hundred per second, so forward whatever has queued up as one batch.
                const MAX_BATCH: usize = 500;

                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
                let finished = run_on_tokio(async move {
                    device
                        .stream_syslog(tx)
                        .await
                        .map_err(|e| format!("Syslog stopped: {e}"))
                });

                while let Some(line) = rx.recv().await {
                    let mut batch = vec![line];
                    while batch.len() < MAX_BATCH {
                        match rx.try_recv() {
                            Ok(line) => batch.push(line),
                            Err(_) => break,
                        }
                    }
                    if output
                        .send(UtilitiesMessage::SyslogLines(batch))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }

                let _ = output
                    .send(UtilitiesMessage::SyslogEnded(finished.await))
                    .await;
            },
        )
    })
}
