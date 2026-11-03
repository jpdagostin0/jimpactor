use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{OnceLock, RwLock};

use idevice::usbmuxd::UsbmuxdAddr;

static OVERRIDE: OnceLock<RwLock<Option<UsbmuxdAddr>>> = OnceLock::new();
static REVISION: AtomicU64 = AtomicU64::new(0);

pub(crate) fn usbmuxd_revision() -> u64 {
    REVISION.load(Ordering::Acquire)
}

pub fn parse_usbmuxd_address(address: &str) -> Option<UsbmuxdAddr> {
    #[cfg(unix)]
    if !address.contains(':') {
        return Some(UsbmuxdAddr::UnixSocket(address.to_owned()));
    }
    address.parse().ok().map(UsbmuxdAddr::TcpSocket)
}

/// The USB service used by device discovery and subsequent operations.
pub fn usbmuxd_address() -> UsbmuxdAddr {
    OVERRIDE
        .get_or_init(RwLock::default)
        .read()
        .expect("USB service address lock poisoned")
        .clone()
        .unwrap_or_else(|| UsbmuxdAddr::from_env_var().unwrap_or_default())
}

/// Changes the USB service for future connections. `None` uses the environment
/// override or the platform default.
pub fn set_usbmuxd_address(address: Option<UsbmuxdAddr>) {
    *OVERRIDE
        .get_or_init(RwLock::default)
        .write()
        .expect("USB service address lock poisoned") = address;
    REVISION.fetch_add(1, Ordering::Release);
}
