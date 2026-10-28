use ::tray::{Icon, TrayIcon, TrayIconBuilder};
use iced::widget::{button, column, container, row, rule, scrollable, text};
use iced::{Element, Point, Size, Task, window};

use crate::screen::Message;

#[derive(Debug, Clone)]
pub enum TrayAction {
    Show,
    Quit,
    RefreshApp { udid: String, app_path: String },
    ForgetApp { udid: String, app_path: String },
}

/// The tray crate's iced-popup example supplies the integration pattern:
/// native icon events open an Iced window, so no GTK/Qt menu backend is needed.
pub(crate) struct ImpactorTray {
    _icon: TrayIcon,
    devices: Vec<plume_store::RefreshDevice>,
    popup: Option<window::Id>,
}

impl ImpactorTray {
    pub(crate) fn new() -> Self {
        #[cfg(target_os = "windows")]
        let bytes = include_bytes!(concat!(env!("OUT_DIR"), "/icon.png"));
        #[cfg(not(target_os = "windows"))]
        let bytes = include_bytes!(concat!(env!("OUT_DIR"), "/tray.png"));
        let image = image::load_from_memory(bytes)
            .expect("generated tray icon is valid")
            .to_rgba8();
        let (width, height) = image.dimensions();
        let icon = TrayIconBuilder::new()
            .with_tooltip(crate::APP_NAME)
            .with_icon(Icon::from_rgba(image.into_raw(), width, height).unwrap())
            .with_icon_as_template(true)
            .build()
            .expect("Failed to create tray icon");
        Self {
            _icon: icon,
            devices: Vec::new(),
            popup: None,
        }
    }

    pub(crate) fn update_refresh_apps(&mut self, store: &plume_store::AccountStore) {
        self.devices = store.refreshes().values().cloned().collect();
    }

    pub(crate) fn open_popup(&mut self, position: Point) -> Task<Message> {
        let close = self.close_popup();
        let apps = self
            .devices
            .iter()
            .map(|device| device.apps.len())
            .sum::<usize>();
        let height = (100.0 + apps as f32 * 110.0).min(420.0);
        let (id, open) = window::open(window::Settings {
            size: Size::new(360.0, height),
            position: window::Position::Specific(Point::new(
                (position.x - 360.0).max(0.0),
                (position.y - height).max(0.0),
            )),
            decorations: false,
            resizable: false,
            level: window::Level::AlwaysOnTop,
            exit_on_close_request: false,
            ..Default::default()
        });
        self.popup = Some(id);
        let position_popup = open.then(move |id| {
            window::scale_factor(id).then(move |scale| {
                window::move_to(
                    id,
                    Point::new(
                        (position.x / scale - 360.0).max(0.0),
                        (position.y / scale - height).max(0.0),
                    ),
                )
            })
        });
        Task::batch([close, position_popup])
    }

    pub(crate) fn is_popup(&self, id: window::Id) -> bool {
        self.popup == Some(id)
    }

    pub(crate) fn close_popup(&mut self) -> Task<Message> {
        self.popup
            .take()
            .map(window::close)
            .unwrap_or_else(Task::none)
    }

    pub(crate) fn view(&self) -> Element<'_, Message> {
        let mut menu = column![
            button("Open")
                .on_press(Message::TrayMenuClicked(TrayAction::Show))
                .width(iced::Fill),
            rule::horizontal(1),
        ]
        .spacing(8);
        for device in &self.devices {
            if device.apps.is_empty() {
                continue;
            }
            menu = menu.push(text(&device.name));
            for app in &device.apps {
                let udid = device.udid.clone();
                let app_path = app.path.to_string_lossy().into_owned();
                menu = menu.push(
                    column![
                        text(format!(
                            "{} (Next: {})",
                            app.name.as_deref().unwrap_or("???"),
                            app.scheduled_refresh.format("%H:%M %b %d")
                        )),
                        row![
                            button("Refresh Now").on_press(Message::TrayMenuClicked(
                                TrayAction::RefreshApp {
                                    udid: udid.clone(),
                                    app_path: app_path.clone()
                                }
                            )),
                            button("Forget App").on_press(Message::TrayMenuClicked(
                                TrayAction::ForgetApp { udid, app_path }
                            )),
                        ]
                        .spacing(8),
                    ]
                    .spacing(4),
                );
            }
            menu = menu.push(rule::horizontal(1));
        }
        menu = menu.push(
            button(text(format!("Quit {}", crate::APP_NAME)))
                .on_press(Message::TrayMenuClicked(TrayAction::Quit))
                .width(iced::Fill),
        );
        container(scrollable(menu)).padding(12).into()
    }
}
