//! Device connections, streaming AFC uploads, installation and profile management.
use crate::{Result, invalid, model::Mobileprovision};
use futures::{StreamExt, stream::BoxStream};
use idevice::{
    IdeviceService,
    afc::{AfcClient, opcode::AfcFopenMode},
    installation_proxy::InstallationProxyClient,
    lockdown::LockdownClient,
    misagent::MisagentClient,
    provider::IdeviceProvider,
    usbmuxd::{Connection as ConnectionType, UsbmuxdConnection, UsbmuxdDevice},
};
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::{Arc, OnceLock, Weak},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Mutex,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SearchMode {
    Usb,
    Network,
    #[default]
    All,
}
impl SearchMode {
    fn allows(self, connection: &ConnectionType) -> bool {
        matches!(
            (self, connection),
            (Self::All, _)
                | (Self::Usb, ConnectionType::Usb)
                | (Self::Network, ConnectionType::Network(_))
        )
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ConnectionPreferences {
    pub lookup_mode: SearchMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionMessage {
    PairDevice,
    UnlockDevice,
}

pub struct Connection {
    pub udid: String,
    pub provider: Arc<dyn IdeviceProvider>,
    pub client: Mutex<LockdownClient>,
}
type ConnectionSlot = Arc<Mutex<Weak<Connection>>>;
type ConnectionPool = Mutex<HashMap<(String, ConnectionPreferences), ConnectionSlot>>;
impl Connection {
    pub async fn connection(
        udid: &str,
        preferences: ConnectionPreferences,
        progress: &(dyn Fn(f64) + Send + Sync),
    ) -> Result<Arc<Self>> {
        Self::connection_with_messages(udid, preferences, progress, &|_| {}).await
    }
    pub async fn connection_with_messages(
        udid: &str,
        preferences: ConnectionPreferences,
        progress: &(dyn Fn(f64) + Send + Sync),
        message: &(dyn Fn(Option<ConnectionMessage>) + Send + Sync),
    ) -> Result<Arc<Self>> {
        static POOL: OnceLock<ConnectionPool> = OnceLock::new();
        progress(0.0);
        let key = (udid.to_owned(), preferences);
        let slot = {
            let mut pool = POOL.get_or_init(Mutex::default).lock().await;
            pool.retain(|_, slot| {
                Arc::strong_count(slot) > 1
                    || slot.try_lock().is_ok_and(|entry| entry.strong_count() != 0)
            });
            pool.entry(key).or_default().clone()
        };
        // Only connection attempts for the same descriptor serialize. Pairing
        // one device cannot block discovery or connection of another device.
        let mut cached = slot.lock().await;
        if let Some(connection) = cached.upgrade() {
            progress(1.0);
            return Ok(connection);
        }
        let mut mux = UsbmuxdConnection::default().await?;
        let device = mux
            .get_devices()
            .await?
            .into_iter()
            .find(|d| d.udid == udid && preferences.lookup_mode.allows(&d.connection_type))
            .ok_or_else(|| invalid(format!("Device '{udid}' was not found")))?;
        progress(0.5);
        let provider = Arc::new(device.to_provider(
            idevice::usbmuxd::UsbmuxdAddr::from_env_var().unwrap_or_default(),
            "xtool",
        ));
        match provider.get_pairing_file().await {
            Ok(_) => (),
            Err(idevice::IdeviceError::UnexpectedResponse(reason))
                if reason == "missing PairRecordData in pair record response" =>
            {
                message(Some(ConnectionMessage::PairDevice));
                Self::pair(provider.as_ref(), &mut mux, udid).await?;
                message(None);
            }
            Err(error) => return Err(error.into()),
        }
        let connection = match Self::with_provider(udid, provider.clone()).await {
            Err(crate::Error::Device(idevice::IdeviceError::InvalidHostID)) => {
                message(Some(ConnectionMessage::PairDevice));
                Self::pair(provider.as_ref(), &mut mux, udid).await?;
                message(None);
                Self::with_provider(udid, provider).await?
            }
            result => result?,
        };
        *cached = Arc::downgrade(&connection);
        progress(1.0);
        Ok(connection)
    }
    async fn pair(
        provider: &dyn IdeviceProvider,
        mux: &mut UsbmuxdConnection,
        udid: &str,
    ) -> Result<()> {
        let mut client = LockdownClient::connect(provider).await?;
        let mut record = client
            .pair(
                uuid::Uuid::new_v4().to_string().to_uppercase(),
                mux.get_buid().await?,
                None,
            )
            .await?;
        record.udid = Some(udid.to_owned());
        mux.save_pair_record(udid, record.serialize()?).await?;
        Ok(())
    }
    /// Custom lookup/tunnel handlers supply a provider and own its lifetime.
    pub async fn with_provider(
        udid: impl Into<String>,
        provider: Arc<dyn IdeviceProvider>,
    ) -> Result<Arc<Self>> {
        let mut client = LockdownClient::connect(provider.as_ref()).await?;
        client
            .start_session(&provider.get_pairing_file().await?)
            .await?;
        Ok(Arc::new(Self {
            udid: udid.into(),
            provider,
            client: Mutex::new(client),
        }))
    }
    pub async fn start_client<T: IdeviceService>(&self) -> Result<T> {
        Ok(T::connect(self.provider.as_ref()).await?)
    }
    pub async fn device_name(&self) -> Result<String> {
        self.client
            .lock()
            .await
            .get_value(Some("DeviceName"), None)
            .await?
            .as_string()
            .map(str::to_owned)
            .ok_or_else(|| invalid("Device name is unavailable"))
    }
    /// Enables wireless connections and returns a separate host pairing record.
    /// The system usbmuxd record is left intact.
    pub async fn configure_wireless(&self) -> Result<Vec<u8>> {
        let mut client = self.client.lock().await;
        client
            .set_value(
                "WirelessBuddyID",
                self.udid.clone().into(),
                Some("com.apple.mobile.wireless_lockdown"),
            )
            .await?;
        client
            .set_value(
                "EnableWifiConnections",
                true.into(),
                Some("com.apple.mobile.wireless_lockdown"),
            )
            .await?;
        let old = self.provider.get_pairing_file().await?;
        let mut buid = uuid::Uuid::parse_str(&old.system_buid)
            .map_err(|e| invalid(e.to_string()))?
            .into_bytes();
        buid[0] = !buid[0];
        let new_buid = uuid::Uuid::from_bytes(buid).to_string().to_uppercase();
        let mut record = client
            .pair(
                uuid::Uuid::new_v4().to_string().to_uppercase(),
                new_buid,
                None,
            )
            .await?;
        record.udid = Some(self.udid.clone());
        Ok(record.serialize()?)
    }
}

#[derive(Clone, Debug)]
pub struct ClientDevice {
    pub device: UsbmuxdDevice,
    pub device_name: String,
}
impl ClientDevice {
    pub fn udid(&self) -> &str {
        &self.device.udid
    }
    pub async fn all(mode: SearchMode) -> Result<Vec<Self>> {
        let mut mux = UsbmuxdConnection::default().await?;
        let mut devices = Vec::new();
        for device in mux.get_devices().await? {
            if !mode.allows(&device.connection_type) {
                continue;
            }
            let provider = device.to_provider(
                idevice::usbmuxd::UsbmuxdAddr::from_env_var().unwrap_or_default(),
                "xtool",
            );
            let mut client = LockdownClient::connect(&provider).await?;
            let device_name = client
                .get_value(Some("DeviceName"), None)
                .await?
                .as_string()
                .map(str::to_owned)
                .ok_or_else(|| invalid("Device name is unavailable"))?;
            devices.push(Self {
                device,
                device_name,
            });
        }
        devices.sort_by_key(|d| {
            (
                d.device.udid.clone(),
                matches!(d.device.connection_type, ConnectionType::Usb),
            )
        });
        Ok(devices)
    }
    /// Emits an initial snapshot, then changed device lists. Dropping the stream
    /// cancels discovery; no detached discovery task is retained.
    pub fn search(mode: SearchMode) -> BoxStream<'static, Result<Vec<Self>>> {
        futures::stream::try_unfold(
            None::<Vec<(String, u32, String)>>,
            move |mut previous| async move {
                loop {
                    if previous.is_some() {
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                    let devices = Self::all(mode).await?;
                    let snapshot = devices
                        .iter()
                        .map(|d| {
                            (
                                d.device.udid.clone(),
                                d.device.device_id,
                                d.device_name.clone(),
                            )
                        })
                        .collect::<Vec<_>>();
                    if previous.as_ref() != Some(&snapshot) {
                        return Ok(Some((devices, Some(snapshot))));
                    }
                    previous = Some(snapshot);
                }
            },
        )
        .boxed()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum InstallationStage {
    Connecting(f64),
    Uploading(f64),
    Installing {
        details: String,
        progress: Option<f64>,
    },
}
impl InstallationStage {
    pub fn display_name(&self) -> &str {
        match self {
            Self::Connecting(_) => "Connecting",
            Self::Uploading(_) => "Installing",
            Self::Installing { .. } => "Verifying",
        }
    }
    pub fn display_progress(&self) -> f64 {
        match self {
            Self::Connecting(v) | Self::Uploading(v) => *v,
            Self::Installing { progress, .. } => progress.unwrap_or(0.0),
        }
    }
}

pub struct IPAUploader {
    client: Arc<Mutex<AfcClient>>,
}
pub struct UploadedIPA {
    pub location: String,
    client: Option<Arc<Mutex<AfcClient>>>,
}
impl UploadedIPA {
    pub async fn delete(&mut self) -> Result<()> {
        if let Some(client) = self.client.as_ref() {
            client.lock().await.remove(&self.location).await?;
            self.client = None;
        }
        Ok(())
    }
}
impl Drop for UploadedIPA {
    fn drop(&mut self) {
        if let (Some(client), Ok(runtime)) =
            (self.client.take(), tokio::runtime::Handle::try_current())
        {
            let location = self.location.clone();
            runtime.spawn(async move {
                let _ = client.lock().await.remove(location).await;
            });
        }
    }
}
impl IPAUploader {
    pub async fn new(connection: &Connection) -> Result<Self> {
        Ok(Self {
            client: Arc::new(Mutex::new(connection.start_client().await?)),
        })
    }
    pub async fn upload(
        &self,
        app: &Path,
        progress: &(dyn Fn(f64) + Send + Sync),
    ) -> Result<UploadedIPA> {
        let mut source = tokio::fs::File::open(app).await?;
        let size = source.metadata().await?.len();
        let mut client = self.client.lock().await;
        if let Err(error) = client.get_file_info("/PublicStaging").await {
            if matches!(
                error,
                idevice::IdeviceError::Afc(idevice::afc::errors::AfcError::ObjectNotFound)
            ) {
                client.mk_dir("/PublicStaging").await?;
            } else {
                return Err(error.into());
            }
        }
        let uploaded = UploadedIPA {
            location: format!("/PublicStaging/XTool-{}.ipa", uuid::Uuid::new_v4()),
            client: Some(self.client.clone()),
        };
        let mut target = client
            .open(&uploaded.location, AfcFopenMode::WrOnly)
            .await?;
        let mut buffer = vec![0; 1 << 20];
        let mut written = 0u64;
        progress(0.0);
        loop {
            let count = source.read(&mut buffer).await?;
            if count == 0 {
                break;
            }
            target.write_all(&buffer[..count]).await?;
            written += count as u64;
            if size != 0 {
                progress((written as f64 / size as f64).min(1.0));
            }
        }
        target.close().await?;
        progress(1.0);
        Ok(uploaded)
    }
}

pub struct IPAInstaller {
    client: InstallationProxyClient,
}
impl IPAInstaller {
    pub async fn new(connection: &Connection) -> Result<Self> {
        Ok(Self {
            client: connection.start_client().await?,
        })
    }
    pub async fn install(
        &mut self,
        uploaded: &UploadedIPA,
        progress: &(dyn Fn(InstallationStage) + Send + Sync),
    ) -> Result<()> {
        self.client
            .install_with_callback(
                &uploaded.location,
                None,
                |(percent, ())| {
                    progress(InstallationStage::Installing {
                        details: "Installing".into(),
                        progress: Some(percent as f64 / 100.0),
                    });
                    std::future::ready(())
                },
                (),
            )
            .await?;
        Ok(())
    }
}

pub struct AppInstaller {
    pub ipa: PathBuf,
    pub udid: String,
    pub connection_preferences: ConnectionPreferences,
}
impl AppInstaller {
    pub async fn install(
        &self,
        progress: &(dyn Fn(InstallationStage) + Send + Sync),
    ) -> Result<()> {
        let connection = Connection::connection(&self.udid, self.connection_preferences, &|v| {
            progress(InstallationStage::Connecting(v * 4.0 / 6.0))
        })
        .await?;
        self.install_with_connection(&connection, progress).await
    }
    pub async fn install_with_connection(
        &self,
        connection: &Connection,
        progress: &(dyn Fn(InstallationStage) + Send + Sync),
    ) -> Result<()> {
        let uploader = IPAUploader::new(connection).await?;
        progress(InstallationStage::Connecting(5.0 / 6.0));
        // Start installation_proxy before uploading to avoid its startup timeout.
        let mut installer = IPAInstaller::new(connection).await?;
        progress(InstallationStage::Connecting(1.0));
        let mut uploaded = uploader
            .upload(&self.ipa, &|v| progress(InstallationStage::Uploading(v)))
            .await?;
        let result = installer.install(&uploaded, progress).await;
        let cleanup = uploaded.delete().await;
        result?;
        cleanup
    }
}

pub struct ProfileManager {
    client: MisagentClient,
    modern: bool,
}
impl ProfileManager {
    pub async fn new(connection: &Connection) -> Result<Self> {
        let version = connection
            .client
            .lock()
            .await
            .get_value(Some("ProductVersion"), None)
            .await?;
        let components = version
            .as_string()
            .unwrap_or("")
            .split('.')
            .filter_map(|v| v.parse::<u32>().ok())
            .collect::<Vec<_>>();
        let modern = components.first().is_some_and(|major| {
            *major > 9 || (*major == 9 && components.get(1).is_some_and(|minor| *minor >= 3))
        });
        Ok(Self {
            client: connection.start_client().await?,
            modern,
        })
    }
    pub async fn all_profiles(&mut self) -> Result<Vec<Mobileprovision>> {
        let data = if self.modern {
            self.client.copy_all().await?
        } else {
            let request = plist::Dictionary::from_iter([
                ("MessageType", plist::Value::from("Copy")),
                ("ProfileType", plist::Value::from("Provisioning")),
            ]);
            let mut bytes = Vec::new();
            plist::to_writer_xml(&mut bytes, &request)?;
            let length: u32 = bytes
                .len()
                .try_into()
                .map_err(|_| invalid("Profile request is too large"))?;
            let mut frame = length.to_be_bytes().to_vec();
            frame.extend_from_slice(&bytes);
            self.client.idevice.send_raw(&frame).await?;
            let length = self.client.idevice.read_raw(4).await?;
            let length = u32::from_be_bytes(
                length
                    .try_into()
                    .map_err(|_| invalid("Invalid profile response length"))?,
            );
            if length > 64 * 1024 * 1024 {
                return Err(invalid("Profile response is too large"));
            }
            let response: plist::Dictionary =
                plist::from_bytes(&self.client.idevice.read_raw(length as usize).await?)?;
            if response
                .get("Status")
                .and_then(plist::Value::as_signed_integer)
                .is_some_and(|status| status != 0)
            {
                return Err(invalid("Could not enumerate legacy provisioning profiles"));
            }
            response
                .get("Payload")
                .and_then(plist::Value::as_array)
                .ok_or_else(|| invalid("Missing profile list"))?
                .iter()
                .map(|value| {
                    value
                        .as_data()
                        .map(<[u8]>::to_vec)
                        .ok_or_else(|| invalid("Invalid profile data"))
                })
                .collect::<Result<Vec<_>>>()?
        };
        data.into_iter().map(Mobileprovision::from_bytes).collect()
    }
    pub async fn install(&mut self, profiles: &[Mobileprovision]) -> Result<()> {
        fn group(profiles: &[Mobileprovision]) -> BTreeMap<String, Vec<&Mobileprovision>> {
            let mut grouped: BTreeMap<String, Vec<&Mobileprovision>> = BTreeMap::new();
            for profile in profiles {
                if let Some(id) = profile
                    .entitlements()
                    .get("application-identifier")
                    .and_then(plist::Value::as_string)
                {
                    grouped.entry(id.to_owned()).or_default().push(profile);
                }
            }
            grouped
        }
        let installed = self.all_profiles().await?;
        let installed = group(&installed);
        for (id, profiles) in group(profiles) {
            if profiles.len() != 1 {
                continue;
            }
            if let Some(old) = installed.get(&id) {
                for profile in old {
                    self.client.remove(&profile.digest().uuid).await?;
                }
            }
            self.client.install(profiles[0].data().to_vec()).await?;
        }
        Ok(())
    }
}

pub struct HeartbeatHandler(tokio::task::JoinHandle<()>);
impl HeartbeatHandler {
    pub async fn new(connection: &Connection) -> Result<Self> {
        let mut client: idevice::heartbeat::HeartbeatClient = connection.start_client().await?;
        let provider = connection.provider.clone();
        Ok(Self(tokio::spawn(async move {
            let mut first = true;
            loop {
                let beat = async {
                    let interval = client.get_marco(if first { 30 } else { 5 }).await?;
                    client.send_polo().await?;
                    first = false;
                    tokio::time::sleep(Duration::from_secs(interval)).await;
                    Ok::<_, idevice::IdeviceError>(())
                }
                .await;
                if beat.is_err() {
                    loop {
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        if let Ok(reconnected) =
                            idevice::heartbeat::HeartbeatClient::connect(provider.as_ref()).await
                        {
                            client = reconnected;
                            first = true;
                            break;
                        }
                    }
                }
            }
        })))
    }
    pub fn stop(&self) {
        self.0.abort();
    }
}
impl Drop for HeartbeatHandler {
    fn drop(&mut self) {
        self.stop();
    }
}

pub struct Debugger {
    client: idevice::debug_proxy::DebugProxyClient<Box<dyn idevice::ReadWrite>>,
}
impl Debugger {
    pub async fn new(connection: &Connection) -> Result<Self> {
        let mut lockdown = connection.client.lock().await;
        let (port, ssl) = lockdown
            .start_service("com.apple.debugserver.DVTSecureSocketProxy")
            .await?;
        let mut socket = connection.provider.connect(port).await?;
        if ssl {
            socket
                .start_session(&connection.provider.get_pairing_file().await?, false)
                .await?;
        }
        let mut client = idevice::debug_proxy::DebugProxyClient::new(
            socket
                .get_socket()
                .ok_or_else(|| invalid("Debugger socket is unavailable"))?,
        );
        client
            .send_command(idevice::debug_proxy::DebugserverCommand::new(
                "QSetMaxPacketSize:".into(),
                vec!["1024".into()],
            ))
            .await?;
        Ok(Self { client })
    }
    pub async fn attach(&mut self, process: &str) -> Result<()> {
        loop {
            let response = self
                .client
                .send_command(idevice::debug_proxy::DebugserverCommand::new(
                    "vAttachWait;".into(),
                    vec![process.into()],
                ))
                .await?;
            if let Some(response) = response.filter(|s| !s.is_empty()) {
                if response.starts_with('E') {
                    return Err(invalid(format!("Debugger attach failed: {response}")));
                }
                break;
            }
            tokio::task::yield_now().await;
        }
        self.client
            .send_command(idevice::debug_proxy::DebugserverCommand::new(
                "D".into(),
                vec![],
            ))
            .await?;
        Ok(())
    }
}
