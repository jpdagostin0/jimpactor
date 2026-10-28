use std::fmt;
use std::path::{Component, Path, PathBuf};

use idevice::core_device_proxy::CoreDeviceProxy;
use idevice::installation_proxy::InstallationProxyClient;
use idevice::lockdown::LockdownClient;
use idevice::misagent::MisagentClient;
use idevice::provider::IdeviceProvider;
use idevice::remote_pairing::{RemotePairingClient, RpPairingFile};
use idevice::rsd::RsdHandshake;
use idevice::usbmuxd::{Connection, UsbmuxdAddr, UsbmuxdDevice};
use idevice::utils::installation;
use idevice::{IdeviceService, RemoteXpcClient};
use plume_core::MobileProvision;

use crate::options::{SignerApp, SignerAppReal};
use crate::{Bundle, Error, NetworkConnection, PlistInfoTrait};
use idevice::afc::opcode::AfcFopenMode;
use idevice::house_arrest::HouseArrestClient;
use idevice::usbmuxd::UsbmuxdConnection;
use plist::Value;

pub const CONNECTION_LABEL: &str = "plume_info";
pub const INSTALLATION_LABEL: &str = "plume_install";
pub const HOUSE_ARREST_LABEL: &str = "plume_house_arrest";

#[derive(Debug, Clone)]
pub struct Device {
    pub name: String,
    pub udid: String,
    pub device_id: u32,
    pub usbmuxd_device: Option<UsbmuxdDevice>,
    pub network: Option<NetworkConnection>,
    // On x86_64 macs, `is_mac` variable should never be true
    // since its only true if the device is added manually.
    pub is_mac: bool,
}

impl Device {
    pub async fn new(usbmuxd_device: UsbmuxdDevice) -> Self {
        Self::with_address(
            usbmuxd_device,
            UsbmuxdAddr::from_env_var().unwrap_or_default(),
        )
        .await
    }

    pub(crate) async fn with_address(usbmuxd_device: UsbmuxdDevice, address: UsbmuxdAddr) -> Self {
        let name = match tokio::time::timeout(
            std::time::Duration::from_secs(3),
            Self::get_name_from_usbmuxd_device(&usbmuxd_device, address),
        )
        .await
        {
            Ok(Ok(name)) if !name.is_empty() => name,
            result => {
                log::warn!(
                    "Could not read device name for {}: {result:?}",
                    usbmuxd_device.udid
                );
                usbmuxd_device.udid.clone()
            }
        };

        Device {
            name,
            udid: usbmuxd_device.udid.clone(),
            device_id: usbmuxd_device.device_id,
            usbmuxd_device: Some(usbmuxd_device),
            network: None,
            is_mac: false,
        }
    }

    /// Connects services through the transport on which this device was found.
    pub fn provider(&self, label: &str) -> Result<Box<dyn IdeviceProvider>, Error> {
        if let Some(network) = &self.network {
            return Ok(Box::new(network.provider(label)));
        }
        self.usbmuxd_device
            .as_ref()
            .map(|device| {
                Box::new(device.to_provider(UsbmuxdAddr::from_env_var().unwrap_or_default(), label))
                    as Box<dyn IdeviceProvider>
            })
            .ok_or_else(|| Error::Other("Device is not connected".into()))
    }

    async fn with_provider<T, F, Fut>(&self, label: &str, operation: F) -> Result<T, Error>
    where
        T: Send + 'static,
        F: FnOnce(Box<dyn IdeviceProvider>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T, Error>> + Send + 'static,
    {
        let result = match &self.network {
            Some(network) => network.execute(label, operation).await,
            None => operation(self.provider(label)?).await,
        };
        result.map_err(|error| match error {
            Error::Idevice(idevice::IdeviceError::Socket(source)) if self.network.is_some() => {
                log::warn!("Wi-Fi operation failed for {}: {source:?}", self.name);
                Error::Other(format!("Wi-Fi connection to {} failed: {source}. Wake and unlock the iPhone and retry.", self.name))
            }
            Error::Idevice(idevice::IdeviceError::UnexpectedResponse(message)) if message.contains("PairRecordData") => {
                Error::Other(format!("No usable pairing record was returned by the Apple device service: {message}. Connect the iPhone over USB, unlock it, accept Trust This Computer, and retry pairing."))
            }
            error => error,
        })
    }

    async fn get_name_from_usbmuxd_device(
        device: &UsbmuxdDevice,
        address: UsbmuxdAddr,
    ) -> Result<String, Error> {
        let mut lockdown =
            LockdownClient::connect(&device.to_provider(address, CONNECTION_LABEL)).await?;
        let value = lockdown.get_value(Some("DeviceName"), None).await?;
        Ok(value.as_string().unwrap_or_default().to_owned())
    }

    pub fn is_available(&self) -> bool {
        self.network.as_ref().map_or(
            self.usbmuxd_device.is_some() || self.is_mac,
            NetworkConnection::is_online,
        )
    }

    pub async fn installed_apps(&self) -> Result<Vec<SignerAppReal>, Error> {
        self.with_provider(INSTALLATION_LABEL, |provider| async move {
            let mut ic = InstallationProxyClient::connect(provider.as_ref()).await?;
            let apps = ic.get_apps(Some("User"), None).await?;

            let profiles = match tokio::time::timeout(std::time::Duration::from_secs(10), async {
                let mut client = MisagentClient::connect(provider.as_ref()).await?;
                client.copy_all().await
            })
            .await
            {
                Ok(Ok(data)) => data
                    .into_iter()
                    .filter_map(|bytes| MobileProvision::load_with_bytes(bytes).ok())
                    .collect::<Vec<_>>(),
                _ => {
                    log::debug!("Installed profile expiration is unavailable");
                    Vec::new()
                }
            };
            let mut found_apps = Vec::new();
            for (bundle_id, info) in apps {
                let app_name = get_app_name_from_info(&info);
                let mut app = SignerAppReal::from_bundle_identifier_and_name(
                    Some(&bundle_id),
                    app_name.as_deref(),
                );
                app.installed_name = app_name;
                app.expiration = installed_profile_expiration(&bundle_id, &info, &profiles);
                found_apps.push(app);
            }
            found_apps.sort_by_key(|app| {
                app.installed_name
                    .clone()
                    .unwrap_or_else(|| app.app.to_string())
                    .to_lowercase()
            });

            Ok(found_apps)
        })
        .await
    }

    pub async fn is_app_installed(&self, bundle_id: &str) -> Result<bool, Error> {
        let bundle_id = bundle_id.to_owned();
        self.with_provider(INSTALLATION_LABEL, move |provider| async move {
            let mut ic = InstallationProxyClient::connect(provider.as_ref()).await?;
            let apps = ic.get_apps(Some("User"), None).await?;

            Ok(apps.contains_key(&bundle_id))
        })
        .await
    }

    pub async fn install_profile(&self, profile: &MobileProvision) -> Result<(), Error> {
        let data = profile.data.clone();
        self.with_provider(INSTALLATION_LABEL, move |provider| async move {
            let mut mc = MisagentClient::connect(provider.as_ref()).await?;
            mc.install(data).await?;

            Ok(())
        })
        .await
    }

    pub async fn pair(&self) -> Result<(), Error> {
        if self.network.is_some() {
            return self
                .with_provider(CONNECTION_LABEL, |provider| async move {
                    let pairing = provider.get_pairing_file().await?;
                    let mut lockdown = LockdownClient::connect(provider.as_ref()).await?;
                    lockdown.start_session(&pairing).await?;
                    Ok(())
                })
                .await;
        }
        if self.usbmuxd_device.is_none() {
            return Err(Error::Other("Device is not connected via USB".to_string()));
        }

        let mut usbmuxd = UsbmuxdConnection::default().await?;

        let provider = self.usbmuxd_device.clone().unwrap().to_provider(
            UsbmuxdAddr::from_env_var().unwrap_or_default(),
            INSTALLATION_LABEL,
        );

        let mut lc = LockdownClient::connect(&provider).await?;
        let id = uuid::Uuid::new_v4().to_string().to_uppercase();
        let buid = usbmuxd.get_buid().await?;
        let mut pairing_file = lc.pair(id, buid, None).await?;
        pairing_file.udid = Some(self.udid.clone());
        let pairing_file = pairing_file.serialize()?;

        usbmuxd.save_pair_record(&self.udid, pairing_file).await.map_err(|error| Error::Other(format!(
            "The Apple device service could not save the pairing record: {error}. Unlock the iPhone, accept Trust This Computer, and retry pairing. If it persists, restart the Apple device service and reconnect USB."
        )))?;

        Ok(())
    }

    /// Returns the lockdown pair record for this device, enabling wifi debugging so
    /// apps can use it over the network.
    pub async fn lockdown_pairing_file(&self) -> Result<Vec<u8>, Error> {
        let udid = self.udid.clone();
        self.with_provider(HOUSE_ARREST_LABEL, move |provider| async move {
            let mut pairing_file = provider.get_pairing_file().await?;

            // saving pairing record requires enabling wifi debugging
            // since operations are done over wifi
            let mut lc = LockdownClient::connect(provider.as_ref()).await?;
            lc.start_session(&pairing_file).await.ok();
            lc.set_value(
                "EnableWifiDebugging",
                true.into(),
                Some("com.apple.mobile.wireless_lockdown"),
            )
            .await
            .ok();

            pairing_file.udid = Some(udid);

            Ok(pairing_file.serialize()?)
        })
        .await
    }

    /// Returns the pairing file to hand to apps. With `remote`, the lockdown pair record is
    /// merged with a remote pairing (RPPairing) record, so apps can pick whichever they need.
    pub async fn pairing_file(
        &self,
        remote: bool,
        path_to_store: PathBuf,
    ) -> Result<Vec<u8>, Error> {
        let lockdown = self.lockdown_pairing_file().await?;
        if !remote {
            return Ok(lockdown);
        }

        let device = self.clone();
        let rppairing = self
            .with_provider(HOUSE_ARREST_LABEL, move |provider| async move {
                Ok(device
                    .get_rsd_pairing_file(provider.as_ref(), path_to_store)
                    .await?
                    .to_bytes())
            })
            .await?;

        merge_pairing_records(&lockdown, &rppairing)
    }

    /// Writes `data` to `path` inside the Documents container of the app `identifier`.
    pub async fn write_app_document(
        &self,
        identifier: &str,
        path: &str,
        data: &[u8],
    ) -> Result<(), Error> {
        let identifier = identifier.to_owned();
        let path = path.to_owned();
        let data = data.to_vec();
        self.with_provider(HOUSE_ARREST_LABEL, move |provider| async move {
            let hc = HouseArrestClient::connect(provider.as_ref()).await?;
            let mut ac = hc.vend_documents(identifier.to_owned()).await?;
            if let Some(parent) = Path::new(&path).parent() {
                let mut current = String::new();
                let has_root = parent.has_root();

                for component in parent.components() {
                    if let Component::Normal(dir) = component {
                        if (has_root && current.is_empty())
                            || (!current.is_empty() && !current.ends_with('/'))
                        {
                            current.push('/');
                        }

                        current.push_str(&dir.to_string_lossy());
                        ac.mk_dir(&current).await?;
                    }
                }
            }

            let mut f = ac.open(&path, AfcFopenMode::Wr).await?;
            f.write_entire(&data).await?;

            Ok(())
        })
        .await
    }

    /// Places the pairing file at every location `app` reads one from.
    pub async fn install_pairing_files(
        &self,
        identifier: &str,
        app: SignerApp,
        remote: bool,
        path_to_store: PathBuf,
    ) -> Result<(), Error> {
        let Some(path) = app.pairing_file_path() else {
            return Err(Error::Other(format!("{app} does not use a pairing file")));
        };

        let data = self.pairing_file(remote, path_to_store).await?;
        for path in std::iter::once(path).chain(app.extra_pairing_file_paths().iter().copied()) {
            self.write_app_document(identifier, path, &data).await?;
        }

        Ok(())
    }

    pub async fn install_pairing_record(&self, identifier: &str, path: &str) -> Result<(), Error> {
        let data = self.lockdown_pairing_file().await?;
        self.write_app_document(identifier, path, &data).await
    }

    pub async fn install_remote_pairing_record(
        &self,
        identifier: &str,
        path: &str,
        path_to_store: PathBuf,
    ) -> Result<(), Error> {
        let device = self.clone();
        let pairing_file = self
            .with_provider(HOUSE_ARREST_LABEL, move |provider| async move {
                Ok(device
                    .get_rsd_pairing_file(provider.as_ref(), path_to_store)
                    .await?
                    .to_bytes())
            })
            .await?;
        self.write_app_document(identifier, path, &pairing_file)
            .await
    }

    async fn get_rsd_pairing_file(
        &self,
        provider: &dyn IdeviceProvider,
        path: PathBuf,
    ) -> Result<RpPairingFile, Error> {
        let pairing_file_path = remote_pairing_cache_path(&path, &self.udid);

        if pairing_file_path.exists() {
            Ok(RpPairingFile::read_from_file(pairing_file_path).await?)
        } else {
            let cdp = CoreDeviceProxy::connect(provider).await?;
            let cdp_port = cdp.tunnel_info().server_rsd_port;
            let cdp_adapter = cdp.create_software_tunnel()?;
            let mut cdp_adapter = cdp_adapter.to_async_handle();

            let cdp_stream = cdp_adapter.connect(cdp_port).await?;
            let cdp_handshake = RsdHandshake::new(cdp_stream).await?;

            let tunnel_service = cdp_handshake
                .services
                .get("com.apple.internal.dt.coredevice.untrusted.tunnelservice")
                .ok_or_else(|| Error::Other("Tunnel service not found".to_string()))?;

            let tunnel_service_stream = cdp_adapter.connect(tunnel_service.port).await?;
            let mut remote_xpc = RemoteXpcClient::new(tunnel_service_stream).await?;
            remote_xpc.do_handshake().await?;
            let _ = remote_xpc.recv_root().await;

            let suffix: String = uuid::Uuid::new_v4()
                .simple()
                .to_string()
                .chars()
                .take(6)
                .collect();

            let hostname = format!("plume-{}", suffix);

            let mut pairing_file = RpPairingFile::generate(&hostname);
            let mut pairing_client =
                RemotePairingClient::new(remote_xpc, &hostname, &mut pairing_file);
            pairing_client
                .connect(async |_| "000000".to_string(), ())
                .await?;

            let tunnel_service_stream = cdp_adapter.connect(tunnel_service.port).await?;
            let mut remote_xpc = RemoteXpcClient::new(tunnel_service_stream).await?;
            remote_xpc.do_handshake().await?;
            let _ = remote_xpc.recv_root().await;
            let mut pairing_client =
                RemotePairingClient::new(remote_xpc, &hostname, &mut pairing_file);
            pairing_client
                .connect(async |_| "000000".to_string(), ())
                .await?;

            let pairing_file_bytes = pairing_file.to_bytes();

            tokio::fs::write(&pairing_file_path, &pairing_file_bytes).await?;

            Ok(pairing_file)
        }
    }

    pub async fn install_app<F, Fut>(
        &self,
        app_path: &Path,
        progress_callback: F,
    ) -> Result<(), Error>
    where
        F: FnMut(i32) -> Fut + Send + Clone + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let bundle = Bundle::new(app_path.to_path_buf())?;
        for bundle in bundle.collect_bundles_sorted()? {
            if !bundle.bundle_type().should_have_entitlements() {
                continue;
            }
            let executable = bundle.get_executable().ok_or_else(|| {
                Error::Other(format!(
                    "Missing CFBundleExecutable in {}",
                    bundle.bundle_dir().display()
                ))
            })?;
            if !bundle.bundle_dir().join(&executable).is_file() {
                return Err(Error::Other(format!(
                    "App executable {executable} is missing from {}",
                    bundle.bundle_dir().display()
                )));
            }
            let path = bundle.bundle_dir().join("embedded.mobileprovision");
            if path.exists() {
                let profile = MobileProvision::load_with_path(path)?;
                let bundle_id = bundle
                    .get_bundle_identifier()
                    .ok_or_else(|| Error::Other("Missing bundle identifier".into()))?;
                profile.validate_for(&bundle_id, Some(&self.udid), None)?;
            }
        }
        let app_path = app_path.to_path_buf();
        self.with_provider(INSTALLATION_LABEL, move |provider| async move {
            let callback = move |(progress, _): (u64, ())| {
                let mut cb = progress_callback.clone();
                async move {
                    cb(progress as i32).await;
                }
            };

            let state = ();

            installation::install_package_with_callback(
                provider.as_ref(),
                &app_path,
                None,
                callback,
                state,
            )
            .await?;

            Ok(())
        })
        .await
    }
}

fn merge_pairing_records(lockdown: &[u8], remote: &[u8]) -> Result<Vec<u8>, Error> {
    let mut merged = Value::from_reader(std::io::Cursor::new(lockdown))?
        .into_dictionary()
        .ok_or_else(|| Error::Other("Lockdown pair record is not a dictionary".into()))?;
    let remote = Value::from_reader(std::io::Cursor::new(remote))?
        .into_dictionary()
        .ok_or_else(|| Error::Other("Remote pair record is not a dictionary".into()))?;
    merged.extend(remote);
    let mut out = Vec::new();
    Value::Dictionary(merged).to_writer_xml(&mut out)?;
    Ok(out)
}

fn installed_profile_expiration(
    bundle_id: &str,
    info: &Value,
    profiles: &[MobileProvision],
) -> Option<plist::Date> {
    let application_id = info
        .as_dictionary()
        .and_then(|info| info.get("Entitlements"))
        .and_then(Value::as_dictionary)
        .and_then(|entitlements| entitlements.get("application-identifier"))
        .and_then(Value::as_string);
    let candidates = profiles
        .iter()
        .filter(|profile| {
            profile.matches_bundle_id(bundle_id)
                && application_id.is_none_or(|application_id| {
                    profile
                        .entitlements()
                        .get("application-identifier")
                        .and_then(Value::as_string)
                        .is_some_and(|pattern| {
                            pattern == application_id
                                || pattern
                                    .strip_suffix('*')
                                    .is_some_and(|prefix| application_id.starts_with(prefix))
                        })
                })
        })
        .collect::<Vec<_>>();
    let exact = candidates
        .iter()
        .copied()
        .filter(|profile| profile.bundle_id().as_deref() == Some(bundle_id))
        .collect::<Vec<_>>();
    let candidates = if exact.is_empty() { candidates } else { exact };
    let date = *candidates.first()?.expiration_date();
    candidates
        .iter()
        .all(|profile| profile.expiration_date() == &date)
        .then_some(date)
}

/// Prefix and suffix of cached remote pairing files, `plume_<UDID>.plist`.
pub const REMOTE_PAIRING_CACHE_PREFIX: &str = "plume_";
pub const REMOTE_PAIRING_CACHE_SUFFIX: &str = ".plist";

/// Where the remote pairing (RPPairing) record for `udid` is cached inside `dir`.
fn remote_pairing_cache_path(dir: &Path, udid: &str) -> PathBuf {
    dir.join(format!(
        "{REMOTE_PAIRING_CACHE_PREFIX}{udid}{REMOTE_PAIRING_CACHE_SUFFIX}"
    ))
}

fn get_app_name_from_info(info: &Value) -> Option<String> {
    let dict = info.as_dictionary()?;
    dict.get("CFBundleDisplayName")
        .and_then(|value| value.as_string())
        .or_else(|| dict.get("CFBundleName").and_then(|value| value.as_string()))
        .or_else(|| {
            dict.get("CFBundleExecutable")
                .and_then(|value| value.as_string())
        })
        .map(|value| value.to_string())
}

impl fmt::Display for Device {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{}] {}",
            match &self.usbmuxd_device {
                Some(device) => match &device.connection_type {
                    Connection::Usb => "USB",
                    Connection::Network(_) => "WiFi",
                    Connection::Unknown(_) => "Unknown",
                },
                None if self.network.is_some() => "WiFi",
                None => "LOCAL",
            },
            self.name
        )
    }
}

pub async fn get_device_for_id(udid: &str) -> Result<Device, Error> {
    let mut usbmuxd = UsbmuxdConnection::default().await?;
    let usbmuxd_device = select_device_by_udid(usbmuxd.get_devices().await?, udid)?;

    Ok(Device::new(usbmuxd_device).await)
}

fn select_device_by_udid(devices: Vec<UsbmuxdDevice>, udid: &str) -> Result<UsbmuxdDevice, Error> {
    devices
        .into_iter()
        .find(|device| device.udid == udid)
        .ok_or_else(|| Error::Other(format!("Device with UDID {udid} not found")))
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub async fn install_app_mac(app_path: &PathBuf) -> Result<(), Error> {
    use crate::copy_dir_recursively;
    use std::env;
    use tokio::fs;
    use uuid::Uuid;

    let stage_dir = env::temp_dir().join(format!(
        "plume_mac_stage_{}",
        Uuid::new_v4().to_string().to_uppercase()
    ));
    let app_name = app_path
        .file_name()
        .ok_or(Error::Other("Invalid app path".to_string()))?;

    // iOS Apps on macOS need to be wrapped in a special structure, more specifically
    // ```
    // LiveContainer.app
    // ├── WrappedBundle -> Wrapper/LiveContainer.app
    // └── Wrapper
    //     └── LiveContainer.app
    // ```
    // Then install to /Applications/...

    let outer_app_dir = stage_dir.join(app_name);
    let wrapper_dir = outer_app_dir.join("Wrapper");

    fs::create_dir_all(&wrapper_dir).await?;

    copy_dir_recursively(app_path, &wrapper_dir.join(app_name)).await?;

    let wrapped_bundle_path = outer_app_dir.join("WrappedBundle");
    fs::symlink(
        PathBuf::from("Wrapper").join(app_name),
        &wrapped_bundle_path,
    )
    .await?;

    let applications_dir = PathBuf::from("/Applications/iOS");
    fs::create_dir_all(&applications_dir).await?;

    let applications_dir = applications_dir.join(app_name);

    fs::remove_dir_all(&applications_dir).await.ok();

    fs::rename(&outer_app_dir, &applications_dir)
        .await
        .map_err(|_| Error::BundleFailedToCopy(applications_dir.to_string_lossy().into_owned()))?;

    Ok(())
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
pub async fn install_app_mac(_app_path: &PathBuf) -> Result<(), Error> {
    Ok(())
}

#[cfg(test)]
mod selection_tests {
    use super::*;
    fn profile(pattern: &str, date: &str) -> MobileProvision {
        let dict = plist::Dictionary::from_iter([
            (
                "ExpirationDate",
                Value::Date(plist::Date::from_xml_format(date).unwrap()),
            ),
            (
                "Entitlements",
                Value::Dictionary(plist::Dictionary::from_iter([(
                    "application-identifier",
                    Value::String(pattern.into()),
                )])),
            ),
        ]);
        let mut bytes = Vec::new();
        Value::Dictionary(dict).to_writer_xml(&mut bytes).unwrap();
        MobileProvision::load_with_bytes(bytes).unwrap()
    }
    #[test]
    fn installed_expiration_prefers_exact_identity_and_does_not_guess_ambiguous_profiles() {
        let profiles = [
            profile("TEAM123456.com.example.*", "2030-01-01T00:00:00Z"),
            profile("TEAM123456.com.example.app", "2031-01-01T00:00:00Z"),
            profile("OTHER12345.com.example.app", "2032-01-01T00:00:00Z"),
        ];
        let info = Value::Dictionary(plist::Dictionary::from_iter([(
            "Entitlements",
            Value::Dictionary(plist::Dictionary::from_iter([(
                "application-identifier",
                Value::String("TEAM123456.com.example.app".into()),
            )])),
        )]));
        assert_eq!(
            installed_profile_expiration("com.example.app", &info, &profiles),
            Some(plist::Date::from_xml_format("2031-01-01T00:00:00Z").unwrap())
        );
        assert!(
            installed_profile_expiration(
                "com.example.app",
                &Value::Dictionary(Default::default()),
                &profiles
            )
            .is_none()
        );
        assert!(installed_profile_expiration("com.other.app", &info, &profiles).is_none());
        assert!(installed_profile_expiration("com.example.app", &info, &[]).is_none());
    }
    #[test]
    fn hybrid_pairing_roundtrips_through_both_consumers() {
        let lockdown = crate::network_connection::tests::fixture_pairing();
        let remote = RpPairingFile::generate("fixture-host");
        let hybrid =
            merge_pairing_records(&lockdown.clone().serialize().unwrap(), &remote.to_bytes())
                .unwrap();
        let legacy = idevice::pairing_file::PairingFile::from_value(
            &Value::from_reader(std::io::Cursor::new(&hybrid)).unwrap(),
        )
        .unwrap();
        assert_eq!(legacy.host_id, lockdown.host_id);
        let parsed_remote = RpPairingFile::from_bytes(&hybrid).unwrap();
        assert_eq!(parsed_remote.public_key_bytes(), remote.public_key_bytes());
        assert_eq!(parsed_remote.identifier(), remote.identifier());
    }
    #[test]
    fn udid_selection_survives_reconnect_and_never_matches_mux_numbers() {
        let devices = |id| {
            vec![
                UsbmuxdDevice {
                    connection_type: Connection::Usb,
                    udid: "target-phone".into(),
                    device_id: id,
                },
                UsbmuxdDevice {
                    connection_type: Connection::Usb,
                    udid: "other-phone".into(),
                    device_id: 1,
                },
            ]
        };
        assert_eq!(
            select_device_by_udid(devices(7), "target-phone")
                .unwrap()
                .device_id,
            7
        );
        assert_eq!(
            select_device_by_udid(devices(23), "target-phone")
                .unwrap()
                .device_id,
            23
        );
        assert!(select_device_by_udid(devices(7), "1").is_err());
    }
}
