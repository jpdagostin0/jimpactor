use plume_utils::Device;

#[derive(Clone, Debug)]
pub(super) struct DeviceLock {
    pub udid: String,
    pub name: String,
}
impl DeviceLock {
    pub fn new(device: &Device) -> Self {
        Self {
            udid: device.udid.clone(),
            name: device.name.clone(),
        }
    }
    pub fn from_saved(saved: &plume_store::LockedDevice) -> Self {
        Self {
            udid: saved.udid.clone(),
            name: saved.name.clone(),
        }
    }
    pub fn to_saved(&self) -> plume_store::LockedDevice {
        plume_store::LockedDevice {
            udid: self.udid.clone(),
            name: self.name.clone(),
        }
    }
    pub fn accepts(&self, device: &Device) -> bool {
        device.udid == self.udid
    }
    pub fn select(&self, devices: &[Device]) -> Option<Device> {
        devices
            .iter()
            .filter(|device| self.accepts(device))
            .max_by_key(|device| device.is_available())
            .cloned()
    }
    pub fn can_install(&self, device: Option<&Device>) -> bool {
        device.is_some_and(|device| self.accepts(device) && device.is_available())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn device(udid: &str, id: u32) -> Device {
        Device {
            name: udid.into(),
            udid: udid.into(),
            device_id: id,
            usbmuxd_device: Some(idevice::usbmuxd::UsbmuxdDevice {
                udid: udid.into(),
                device_id: id,
                connection_type: idevice::usbmuxd::Connection::Usb,
            }),
            network: None,
            is_mac: false,
        }
    }
    #[test]
    fn locked_target_does_not_fall_back_to_another_phone_and_survives_a_new_mux_id() {
        let lock = DeviceLock::new(&device("target", 1));
        let other = device("other", 2);
        assert!(lock.select(std::slice::from_ref(&other)).is_none());
        assert!(!lock.can_install(Some(&other)));
        assert!(!lock.can_install(None));
        let reconnected = device("target", 25);
        let selected = lock.select(&[other, reconnected]).unwrap();
        assert_eq!(selected.device_id, 25);
        assert!(lock.can_install(Some(&selected)));
        let mut offline = selected;
        offline.usbmuxd_device = None;
        assert!(!lock.can_install(Some(&offline)));
    }
}
