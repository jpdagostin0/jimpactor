//! Answers IPC requests from other programs (see `plume_utils::ipc`), installing IPAs in the
//! background the same way the tray refreshes apps.

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use plume_store::AccountStore;
use plume_utils::ipc::{DeviceSummary, Event, Request};
use plume_utils::{Device, Package, PlistInfoTrait};

use crate::defaults::get_data_path;
use crate::relaunch::RequestHandler;

static ACTIVE_INSTALLS: AtomicUsize = AtomicUsize::new(0);
/// UDIDs with an IPC install running, so a device gets one install at a time.
static BUSY: Mutex<Option<HashSet<String>>> = Mutex::new(None);

pub(crate) fn has_active_installs() -> bool {
    ACTIVE_INSTALLS.load(Ordering::SeqCst) != 0
}

pub(crate) fn handler() -> RequestHandler {
    Arc::new(|request, emit| match request {
        Request::Info => emit(Event::Info {
            app_version: env!("CARGO_PKG_VERSION").into(),
            protocol_version: plume_utils::ipc::PROTOCOL_VERSION,
        }),
        Request::ListDevices => {
            let mut devices: Vec<DeviceSummary> = connected_devices()
                .iter()
                .map(DeviceSummary::from)
                .collect();
            devices.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.udid.cmp(&b.udid)));
            emit(Event::Devices { devices });
        }
        Request::Install { path, udid } => install(&path, udid.as_deref(), emit),
    })
}

fn connected_devices() -> Vec<Device> {
    crate::screen::refresh_daemon_devices()
        .and_then(|devices| devices.lock().ok().map(|d| d.values().cloned().collect()))
        .unwrap_or_default()
}

/// Picks the install target: `udid` if given, else the locked device, else the only device.
fn select_device(
    devices: &[Device],
    udid: Option<&str>,
    locked: Option<&str>,
) -> Result<Device, String> {
    let wanted = udid.or(locked);
    if let Some(wanted) = wanted {
        let device = devices
            .iter()
            .find(|device| device.udid == wanted)
            .ok_or_else(|| format!("Device {wanted} is not connected"))?;
        if !device.is_available() {
            return Err(format!("{} is not reachable right now", device.name));
        }
        return Ok(device.clone());
    }
    let candidates: Vec<&Device> = devices
        .iter()
        .filter(|device| !device.is_mac && device.is_available())
        .collect();
    match candidates.as_slice() {
        [device] => Ok((*device).clone()),
        [] => Err("No device is connected".into()),
        many => Err(format!(
            "Several devices are connected; choose one by UDID: {}",
            many.iter()
                .map(|device| format!("{} ({})", device.name, device.udid))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

struct InstallGuard(String);

impl InstallGuard {
    fn acquire(udid: &str) -> Option<Self> {
        let mut busy = BUSY.lock().ok()?;
        if !busy.get_or_insert_default().insert(udid.to_owned()) {
            return None;
        }
        ACTIVE_INSTALLS.fetch_add(1, Ordering::SeqCst);
        Some(Self(udid.to_owned()))
    }
}

impl Drop for InstallGuard {
    fn drop(&mut self) {
        ACTIVE_INSTALLS.fetch_sub(1, Ordering::SeqCst);
        if let Ok(mut busy) = BUSY.lock()
            && let Some(busy) = busy.as_mut()
        {
            busy.remove(&self.0);
        }
    }
}

struct IpcInstallHost {
    tx: std::sync::mpsc::Sender<Event>,
}

impl plume_utils::install::InstallHost for IpcInstallHost {
    fn status(&self, message: String, percent: i32) {
        let _ = self.tx.send(Event::Progress {
            message,
            percent: percent.clamp(0, 100),
        });
    }

    fn confirm_certificate_reset(&self) -> bool {
        crate::certificate_reset::confirm()
    }

    fn export_path(
        &self,
        _suggested_name: String,
    ) -> futures::future::BoxFuture<'_, Option<std::path::PathBuf>> {
        Box::pin(async { None })
    }
}

fn notify(body: String) {
    notify_rust::Notification::new()
        .summary(crate::APP_NAME)
        .body(&body)
        .show()
        .ok();
}

fn install(path: &Path, udid: Option<&str>, emit: &mut dyn FnMut(Event)) {
    let data_path = get_data_path();
    let store = match AccountStore::load_sync(&Some(data_path.join("accounts.json"))) {
        Ok(store) => store,
        Err(error) => return emit(Event::error(format!("Could not load settings: {error}"))),
    };
    let result = check_request(path, udid, &store, &connected_devices()).and_then(|device| {
        let guard = InstallGuard::acquire(&device.udid)
            .ok_or_else(|| format!("{} is already installing an app", device.name))?;
        let package = Package::new(path.to_path_buf())
            .map_err(|e| format!("{} is not a valid IPA: {e}", path.display()))?;
        Ok((device, package, guard))
    });
    let (device, package, guard) = match result {
        Ok(prepared) => prepared,
        Err(error) => return emit(Event::error(error)),
    };

    let app = package
        .get_name()
        .unwrap_or_else(|| path.display().to_string());
    log::info!("IPC request: installing {app} on {}", device.name);
    notify(format!("Installing {app} on {}", device.name));

    let (tx, rx) = std::sync::mpsc::channel();
    let worker = {
        let device = device.clone();
        std::thread::spawn(move || {
            let _guard = guard;
            let mut store = store;
            let account = store.selected_account().cloned();
            let host: Arc<dyn plume_utils::install::InstallHost> =
                Arc::new(IpcInstallHost { tx: tx.clone() });
            let result = tokio::runtime::Runtime::new()
                .map_err(|e| e.to_string())
                .and_then(|runtime| {
                    runtime.block_on(plume_utils::install::install_with_saved_options(
                        package,
                        Some(&device),
                        account.as_ref(),
                        Some(&mut store),
                        &data_path,
                        &host,
                    ))
                });
            let _ = tx.send(match result {
                Ok(()) => Event::Done {
                    message: format!("Installed {app} on {}", device.name),
                },
                Err(error) => Event::error(error),
            });
        })
    };

    // The worker's sender closes once it finishes, ending this loop.
    for event in rx {
        match &event {
            Event::Done { message } => {
                log::info!("{message}");
                notify(message.clone());
            }
            Event::Error { message } => {
                log::error!("IPC install on {} failed: {message}", device.name);
                notify(format!("Install on {} failed: {message}", device.name));
            }
            _ => {}
        }
        emit(event);
    }
    if worker.join().is_err() {
        emit(Event::error("The installation stopped unexpectedly"));
    }
}

/// Checks the request and picks its device. Errors are meant for the client.
fn check_request(
    path: &Path,
    udid: Option<&str>,
    store: &AccountStore,
    devices: &[Device],
) -> Result<Device, String> {
    if !path.is_absolute() {
        return Err(format!("{} is not an absolute path", path.display()));
    }
    if !path.is_file() {
        return Err(format!("{} does not exist", path.display()));
    }
    if store.selected_account().is_none() && store.asc_key().is_none() {
        return Err("Sign in to Jimpactor before installing apps".into());
    }
    let locked = store
        .lock_device()
        .then(|| store.locked_device().map(|device| device.udid.as_str()))
        .flatten();
    select_device(devices, udid, locked)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(udid: &str, is_mac: bool) -> Device {
        Device {
            name: format!("{udid} name"),
            udid: udid.into(),
            device_id: 1,
            usbmuxd_device: Some(idevice::usbmuxd::UsbmuxdDevice {
                udid: udid.into(),
                device_id: 1,
                connection_type: idevice::usbmuxd::Connection::Usb,
            }),
            usbmuxd_address: None,
            network: None,
            is_mac,
        }
    }

    #[test]
    fn device_is_chosen_by_udid_then_lock_then_when_it_is_the_only_one() {
        let one = [device("phone", false), device("mac", true)];
        assert_eq!(select_device(&one, None, None).unwrap().udid, "phone");
        assert_eq!(select_device(&one, Some("mac"), None).unwrap().udid, "mac");

        let two = [device("a", false), device("b", false)];
        let error = select_device(&two, None, None).unwrap_err();
        assert!(error.contains("a name (a)") && error.contains("b name (b)"));
        assert_eq!(select_device(&two, None, Some("b")).unwrap().udid, "b");
        assert_eq!(select_device(&two, Some("a"), Some("b")).unwrap().udid, "a");
        assert!(select_device(&two, Some("c"), None).is_err());
        assert!(select_device(&[], None, None).is_err());
    }

    #[test]
    fn a_device_takes_one_install_at_a_time() {
        let first = InstallGuard::acquire("ipc-test-device").unwrap();
        assert!(has_active_installs());
        assert!(InstallGuard::acquire("ipc-test-device").is_none());
        drop(first);
        assert!(InstallGuard::acquire("ipc-test-device").is_some());
    }

    #[test]
    fn bad_requests_are_rejected_before_installing() {
        let store = AccountStore::default();
        let devices = [device("phone", false)];
        let ipa = std::env::temp_dir().join(format!("ipc-test-{}.ipa", std::process::id()));
        std::fs::write(&ipa, b"not a zip").unwrap();
        let check = |path: &Path| check_request(path, None, &store, &devices);
        assert!(
            check(Path::new("relative.ipa"))
                .unwrap_err()
                .contains("absolute")
        );
        assert!(
            check(Path::new("/nonexistent/App.ipa"))
                .unwrap_err()
                .contains("does not exist")
        );
        assert!(check(&ipa).unwrap_err().contains("Sign in"));
        std::fs::remove_file(&ipa).ok();
    }
}
