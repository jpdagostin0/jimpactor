#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

rust_i18n::i18n!("../../locales", fallback = "en");

use crate::refresh::spawn_refresh_daemon;

mod appearance;
mod certificate_reset;
mod defaults;
mod jit_server;
mod logging;
mod macos_app;
mod refresh;
mod relaunch;
mod screen;
mod sources;
mod startup;
mod subscriptions;
mod tray;

pub const APP_NAME: &str = "Jimpactor";
pub const APP_NAME_VERSIONED: &str = concat!("Jimpactor", " - Version ", env!("CARGO_PKG_VERSION"));

fn main() -> iced::Result {
    logging::init();
    log::info!(
        "Starting {} on {} ({})",
        APP_NAME_VERSIONED,
        std::env::consts::OS,
        std::env::consts::ARCH
    );

    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();

    if let Ok(locale) = current_locale::current_locale() {
        log::info!("Setting locale to {locale}...");
        rust_i18n::set_locale(&locale);
    }

    #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
    let _single_instance = match crate::relaunch::acquire_or_notify() {
        Ok(Some(instance)) => instance,
        Ok(None) => return Ok(()),
        Err(error) => {
            log::error!("Could not select the running Jimpactor version: {error}");
            rfd::MessageDialog::new()
                .set_title("Could not start Jimpactor")
                .set_description(&error)
                .set_level(rfd::MessageLevel::Error)
                .show();
            return Ok(());
        }
    };

    // For notifications on macOS.
    #[cfg(target_os = "macos")]
    {
        notify_rust::get_bundle_identifier_or_default("Jimpactor");
        notify_rust::set_application("io.github.jpdagostin0.jimpactor").ok();
    }

    let (_daemon_handle, connected_devices) = spawn_refresh_daemon();
    screen::set_refresh_daemon_devices(connected_devices);

    // We're going to try and try running the iced_daemon with different
    // environment variables so it can run properly
    // RE: https://github.com/claration/Impactor/issues/103, https://github.com/claration/Impactor/issues/90
    #[cfg(target_os = "linux")]
    check_gpu();

    let result = run_daemon();
    if let Err(error) = &result {
        log::error!("Application startup failed: {error}");
        log::logger().flush();
        rfd::MessageDialog::new()
            .set_title("Jimpactor could not start")
            .set_description(format!("{error}\n\nLogs: {}", logging::log_dir().display()))
            .set_level(rfd::MessageLevel::Error)
            .show();
    }
    result
}

#[cfg(target_os = "linux")]
fn check_gpu() {
    let instance = wgpu::Instance::default();

    let adapter =
        iced::futures::executor::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: false,
        }));

    match adapter {
        Ok(adapter) => {
            if !adapter.features().contains(wgpu::Features::SHADER_F16) {
                log::warn!("No FP16 support, falling back to tiny-skia");
                unsafe {
                    std::env::set_var("ICED_BACKEND", "tiny-skia");
                }
            }
        }
        Err(e) => {
            log::warn!("No adapter found: {e}, falling back to tiny-skia");
            unsafe {
                std::env::set_var("ICED_BACKEND", "tiny-skia");
            }
        }
    }
}

fn run_daemon() -> iced::Result {
    iced::daemon(
        screen::Impactor::new,
        screen::Impactor::update,
        screen::Impactor::view,
    )
    .subscription(screen::Impactor::subscription)
    .title(APP_NAME_VERSIONED)
    .theme(screen::Impactor::theme)
    .settings(defaults::default_settings())
    .run()
}
