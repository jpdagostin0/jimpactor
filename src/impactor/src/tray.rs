use std::collections::HashMap;

use tray_icon::menu::{Menu, MenuId, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

#[derive(Debug, Clone)]
pub enum TrayAction {
    Show,
    Quit,
    RefreshApp { udid: String, app_path: String },
    ForgetApp { udid: String, app_path: String },
}

/// Native tray menus do not create an Iced window or a graphics surface.
pub(crate) struct ImpactorTray {
    icon: TrayIcon,
    actions: HashMap<MenuId, TrayAction>,
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
        let icon = Icon::from_rgba(image.into_raw(), width, height).unwrap();
        let (menu, actions) = build_menu(&[]).expect("Failed to create tray menu");
        let builder = TrayIconBuilder::new()
            .with_tooltip(crate::APP_NAME)
            .with_menu(Box::new(menu));
        #[cfg(target_os = "macos")]
        let builder = builder.with_icon_templated(icon);
        #[cfg(not(target_os = "macos"))]
        let builder = builder.with_icon(icon);
        let icon = builder.build().expect("Failed to create tray icon");
        Self { icon, actions }
    }

    pub(crate) fn update_refresh_apps(&mut self, store: &plume_store::AccountStore) {
        let devices: Vec<_> = store.refreshes().values().cloned().collect();
        match build_menu(&devices) {
            Ok((menu, actions)) => {
                self.icon.set_menu(Some(Box::new(menu)));
                self.actions = actions;
            }
            Err(error) => log::warn!("Failed to update tray menu: {error}"),
        }
    }

    pub(crate) fn action(&self, id: &MenuId) -> Option<TrayAction> {
        self.actions.get(id).cloned()
    }
}

fn build_menu(
    devices: &[plume_store::RefreshDevice],
) -> Result<(Menu, HashMap<MenuId, TrayAction>), tray_icon::menu::Error> {
    let menu = Menu::new();
    let mut actions = HashMap::new();
    let open = MenuItem::new("Open", true, None);
    actions.insert(open.id().clone(), TrayAction::Show);
    menu.append(&open)?;
    if devices.iter().any(|device| !device.apps.is_empty()) {
        menu.append(&PredefinedMenuItem::separator())?;
    }

    for device in devices.iter().filter(|device| !device.apps.is_empty()) {
        let device_menu = Submenu::new(&device.name, true);
        for app in &device.apps {
            let app_menu = Submenu::new(
                format!(
                    "{} (Next: {})",
                    app.name.as_deref().unwrap_or("???"),
                    app.scheduled_refresh.format("%H:%M %b %d")
                ),
                true,
            );
            let refresh = MenuItem::new("Refresh Now", true, None);
            let forget = MenuItem::new("Forget App", true, None);
            let udid = device.udid.clone();
            let app_path = app.path.to_string_lossy().into_owned();
            actions.insert(
                refresh.id().clone(),
                TrayAction::RefreshApp {
                    udid: udid.clone(),
                    app_path: app_path.clone(),
                },
            );
            actions.insert(
                forget.id().clone(),
                TrayAction::ForgetApp { udid, app_path },
            );
            app_menu.append(&refresh)?;
            app_menu.append(&forget)?;
            device_menu.append(&app_menu)?;
        }
        menu.append(&device_menu)?;
    }

    menu.append(&PredefinedMenuItem::separator())?;
    let quit = MenuItem::new(format!("Quit {}", crate::APP_NAME), true, None);
    actions.insert(quit.id().clone(), TrayAction::Quit);
    menu.append(&quit)?;
    Ok((menu, actions))
}

// AppKit menu construction requires the main thread, unlike Rust's test runner.
#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use super::*;
    use plume_store::{RefreshApp, RefreshDevice};

    #[test]
    fn empty_menu_keeps_open_and_quit() {
        let (menu, actions) = build_menu(&[]).unwrap();
        let items = menu.items();
        assert_eq!(items.len(), 3);
        assert!(matches!(actions.get(items[0].id()), Some(TrayAction::Show)));
        assert!(matches!(
            actions.get(items.last().unwrap().id()),
            Some(TrayAction::Quit)
        ));
        assert_eq!(actions.len(), 2);
    }

    #[test]
    fn app_actions_target_their_device_and_expire_when_menu_is_rebuilt() {
        let devices: Vec<_> = ["phone", "tablet"]
            .into_iter()
            .map(|udid| RefreshDevice {
                udid: udid.into(),
                name: udid.into(),
                account: "test@example.com".into(),
                is_mac: false,
                apps: vec![RefreshApp {
                    path: format!("{udid}/app.ipa").into(),
                    name: Some("Same app name".into()),
                    bundle_id: None,
                    scheduled_refresh: "2026-10-28T12:00:00Z".parse().unwrap(),
                }],
            })
            .collect();
        let (menu, actions) = build_menu(&devices).unwrap();
        for (item, device) in menu.items()[2..4].iter().zip(&devices) {
            let device_menu = item.as_submenu().unwrap();
            let app_items = device_menu.items();
            let app_menu = app_items[0].as_submenu().unwrap();
            assert!(app_menu.text().contains("Next: 12:00 Oct 28"));
            let items = app_menu.items();
            let path = device.apps[0].path.to_string_lossy();
            assert!(matches!(
                actions.get(items[0].id()),
                Some(TrayAction::RefreshApp { udid, app_path })
                    if udid == &device.udid && app_path == &path
            ));
            assert!(matches!(
                actions.get(items[1].id()),
                Some(TrayAction::ForgetApp { udid, app_path })
                    if udid == &device.udid && app_path == &path
            ));
        }
        let (_, replacement_actions) = build_menu(&devices).unwrap();
        assert!(
            actions
                .keys()
                .all(|id| !replacement_actions.contains_key(id))
        );
        let (_, empty_actions) = build_menu(&[]).unwrap();
        assert!(
            empty_actions
                .values()
                .all(|action| matches!(action, TrayAction::Show | TrayAction::Quit))
        );
    }
}
