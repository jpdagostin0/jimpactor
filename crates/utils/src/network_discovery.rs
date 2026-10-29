//! Apple Wi-Fi discovery via DNS-SD and the host's existing lockdown pairings.

use std::collections::HashMap;
use std::net::{SocketAddr, SocketAddrV6};
use std::path::{Path, PathBuf};
use std::time::Duration;

use idevice::heartbeat::HeartbeatClient;
use idevice::lockdown::LockdownClient;
use idevice::pairing_file::PairingFile;
use idevice::usbmuxd::UsbmuxdAddr;
use idevice::{IdeviceError, IdeviceService};
use mdns_sd::{ResolvedService, ScopedIp, ServiceDaemon, ServiceEvent};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::time::Instant;

use crate::device::CONNECTION_LABEL;
use crate::device_worker::DeviceWorker;
use crate::network_connection::{NetworkConnections, NetworkProvider, Route};
use crate::{Device, DeviceEvent, Error, NetworkConnection};

const APPLE_MOBILE_SERVICE: &str = "_apple-mobdev2._tcp.local.";
const WIFI_DEVICE_CACHE_DURATION: Duration = Duration::from_secs(5 * 60);

struct CachedDevice {
    display_id: u32,
    connection_id: u32,
    expires: Option<Instant>,
    device: Device,
}

#[derive(Default)]
struct WifiDeviceCache {
    devices: HashMap<String, CachedDevice>,
    connections: HashMap<u32, String>,
}

impl WifiDeviceCache {
    fn connected(&mut self, mut device: Device) -> Device {
        let cached = self
            .devices
            .entry(device.udid.clone())
            .or_insert(CachedDevice {
                display_id: device.device_id,
                connection_id: device.device_id,
                expires: None,
                device: device.clone(),
            });
        self.connections.remove(&cached.connection_id);
        cached.connection_id = device.device_id;
        cached.expires = None;
        self.connections
            .insert(device.device_id, device.udid.clone());
        // Reuse the list entry and selection even if DNS-SD assigned a new ID.
        device.device_id = cached.display_id;
        cached.device = device.clone();
        device
    }

    fn disconnected(&mut self, id: u32, now: Instant) -> bool {
        if let Some(udid) = self.connections.get(&id)
            && let Some(cached) = self.devices.get_mut(udid)
        {
            // Repeated failures must not extend an offline device's lifetime.
            let first_disconnect = cached.expires.is_none();
            cached
                .expires
                .get_or_insert(now + WIFI_DEVICE_CACHE_DURATION);
            return first_disconnect;
        }
        false
    }

    fn next_expiration(&self) -> Option<Instant> {
        self.devices
            .values()
            .filter_map(|device| device.expires)
            .min()
    }

    fn expire(&mut self, now: Instant) -> Vec<u32> {
        let mut expired = Vec::new();
        self.devices.retain(|_, device| {
            if device.expires.is_some_and(|deadline| deadline <= now) {
                self.connections.remove(&device.connection_id);
                expired.push(device.display_id);
                false
            } else {
                true
            }
        });
        expired
    }
}

pub(crate) async fn forward_cached_devices(
    mut events: UnboundedReceiver<DeviceEvent>,
    output: UnboundedSender<DeviceEvent>,
) {
    let mut cache = WifiDeviceCache::default();
    loop {
        let expiration = cache.next_expiration();
        tokio::select! {
            _ = output.closed() => return,
            event = events.recv() => match event {
                Some(DeviceEvent::Connected(device)) => {
                    if output.send(DeviceEvent::Connected(cache.connected(device))).is_err() {
                        return;
                    }
                }
                Some(DeviceEvent::Disconnected(id)) => {
                    if cache.disconnected(id, Instant::now()) {
                        let device = cache.connections.get(&id).and_then(|udid| cache.devices.get(udid)).map(|cached| cached.device.clone());
                        if let Some(device) = device { let _ = output.send(DeviceEvent::Connected(device)); }
                    }
                },
                None => return,
            },
            _ = async {
                match expiration {
                    Some(deadline) => tokio::time::sleep_until(deadline).await,
                    None => std::future::pending().await,
                }
            } => {
                for id in cache.expire(Instant::now()) {
                    log::info!("Wi-Fi device {id} expired after five minutes disconnected");
                    if output.send(DeviceEvent::Disconnected(id)).is_err() {
                        return;
                    }
                }
            }
        }
    }
}

struct Browser(ServiceDaemon);

impl Drop for Browser {
    fn drop(&mut self) {
        let _ = self.0.shutdown();
    }
}

struct NetworkService {
    id: u32,
    addresses: Vec<SocketAddr>,
    task: DeviceWorker,
}

/// Discovers paired iPhones/iPads directly on the LAN, independently of whether
/// Apple's USB service publishes network devices. DNS-SD advertisements only
/// locate endpoints; a saved pairing must authenticate before a device is shown.
/// Previously authenticated devices remain in the list for five minutes after
/// losing their connection, retaining their selection when they reconnect.
pub async fn watch_network_devices(output: UnboundedSender<DeviceEvent>) {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::join!(
        watch_network_connections(tx),
        forward_cached_devices(rx, output)
    );
}

async fn watch_network_connections(output: UnboundedSender<DeviceEvent>) {
    // Keep raw IDs unique across browser restarts so late disconnects cannot
    // invalidate another device's cached entry.
    let mut next_id = 0x8000_0000u32;
    let connections = NetworkConnections::default();
    loop {
        tokio::select! {
            _ = output.closed() => return,
            result = watch_browser(&output, &mut next_id, &connections) => {
                log::warn!("Wi-Fi discovery interrupted: {result:?}; retrying in 2 seconds");
            }
        }
        tokio::select! {
            _ = output.closed() => return,
            _ = tokio::time::sleep(Duration::from_secs(2)) => {}
        }
    }
}

async fn watch_browser(
    output: &UnboundedSender<DeviceEvent>,
    next_id: &mut u32,
    connections: &NetworkConnections,
) -> Result<(), Error> {
    let browser = Browser(ServiceDaemon::new().map_err(|error| Error::Other(error.to_string()))?);
    let events = browser
        .0
        .browse(APPLE_MOBILE_SERVICE)
        .map_err(|error| Error::Other(error.to_string()))?;
    log::info!("Browsing for paired Wi-Fi devices on {APPLE_MOBILE_SERVICE}");
    let mut services = HashMap::<String, NetworkService>::new();
    let result = loop {
        match events.recv_async().await {
            Ok(ServiceEvent::ServiceResolved(service)) => {
                let addresses = service_addresses(&service);
                if addresses.is_empty()
                    || services
                        .get(service.get_fullname())
                        .is_some_and(|previous| previous.addresses == addresses)
                {
                    continue;
                }
                let id = if let Some(previous) = services.remove(service.get_fullname()) {
                    previous.task.shutdown().await;
                    let _ = output.send(DeviceEvent::Disconnected(previous.id));
                    previous.id
                } else {
                    let id = *next_id;
                    *next_id = next_id
                        .checked_add(1)
                        .filter(|id| *id != u32::MAX)
                        .unwrap_or(0x8000_0000);
                    id
                };
                let name = service.get_fullname().to_owned();
                log::debug!("Resolved Apple Wi-Fi service {name} at {addresses:?}");
                let worker_name = name.clone();
                let worker_addresses = addresses.clone();
                let worker_output = output.clone();
                let connections = connections.clone();
                let task = DeviceWorker::spawn(format!("jimpactor-wifi-device-{id}"), move || {
                    run_network_device(
                        worker_name,
                        worker_addresses,
                        id,
                        worker_output,
                        connections,
                    )
                })?;
                services.insert(
                    name,
                    NetworkService {
                        id,
                        addresses,
                        task,
                    },
                );
            }
            Ok(ServiceEvent::ServiceRemoved(_, name)) => {
                if let Some(service) = services.remove(&name) {
                    service.task.shutdown().await;
                    let _ = output.send(DeviceEvent::Disconnected(service.id));
                }
            }
            Ok(_) => {}
            Err(error) => break Err(Error::Other(format!("mDNS browser closed: {error}"))),
        }
    };
    for service in services.into_values() {
        service.task.shutdown().await;
        let _ = output.send(DeviceEvent::Disconnected(service.id));
    }
    result
}

fn service_addresses(service: &ResolvedService) -> Vec<SocketAddr> {
    let mut addresses: Vec<_> = service
        .get_addresses()
        .iter()
        .map(|address| match address {
            ScopedIp::V6(address) => SocketAddr::V6(SocketAddrV6::new(
                *address.addr(),
                LockdownClient::LOCKDOWND_PORT,
                0,
                address.scope_id().index,
            )),
            address => SocketAddr::new(address.to_ip_addr(), LockdownClient::LOCKDOWND_PORT),
        })
        .collect();
    // Prefer IPv4, while retaining scoped IPv6 for networks without IPv4.
    addresses.sort_by_key(|address| (address.is_ipv6(), *address));
    addresses.dedup();
    addresses
}

async fn run_network_device(
    service: String,
    addresses: Vec<SocketAddr>,
    id: u32,
    output: UnboundedSender<DeviceEvent>,
    connections: NetworkConnections,
) {
    log::info!(
        "Device worker started on {}",
        std::thread::current().name().unwrap_or("unnamed thread")
    );
    loop {
        match connect_network_device(&service, &addresses, id, Route::Lan).await {
            Ok((mut device, mut heartbeat, lockdown)) => {
                let provider = device
                    .network
                    .as_ref()
                    .expect("initialized network transport")
                    .provider(CONNECTION_LABEL);
                let (connection, session) = connections.connected(&device.udid, provider);
                device.network = Some(connection);
                // Retain the authenticated lockdown session alongside heartbeat.
                let _lockdown = lockdown;
                log::info!("Wi-Fi device connected: {device}");
                if output.send(DeviceEvent::Connected(device)).is_err() {
                    return;
                }
                let result = keep_alive(&mut heartbeat).await;
                let replaced = !session.is_current();
                drop(session);
                log::debug!("Wi-Fi connection ended for {service}: {result:?}");
                if output.send(DeviceEvent::Disconnected(id)).is_err() {
                    return;
                }
                if replaced {
                    // A newer advertisement already owns this device. Do not
                    // reconnect the old worker and compete for its heartbeat.
                    return;
                }
            }
            Err(error) => log::debug!("Wi-Fi device {service} is not ready: {error}"),
        }
        tokio::select! {
            _ = output.closed() => return,
            _ = tokio::time::sleep(Duration::from_secs(5)) => {}
        }
    }
}

pub(crate) async fn keep_alive(heartbeat: &mut HeartbeatClient) -> Result<(), IdeviceError> {
    let mut interval = 30;
    loop {
        interval = heartbeat.get_marco(interval + 5).await?.clamp(1, 60);
        heartbeat.send_polo().await?;
    }
}

fn normalize_mac(value: &str) -> String {
    value
        .chars()
        .filter(char::is_ascii_hexdigit)
        .flat_map(char::to_lowercase)
        .collect()
}

fn pairing_directories() -> Vec<PathBuf> {
    if let Some(path) =
        std::env::var_os("USBMUXD_PAIRING_FILES_LOCATION").filter(|value| !value.is_empty())
    {
        return vec![path.into()];
    }
    #[cfg(windows)]
    {
        let base = std::env::var_os("PROGRAMDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
        vec![
            base.join("Apple/Lockdown"),
            base.join("Apple Computer/Lockdown"),
        ]
    }
    #[cfg(target_os = "macos")]
    {
        vec![PathBuf::from("/var/db/lockdown")]
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        vec![PathBuf::from("/var/lib/lockdown")]
    }
}

fn read_pairings(directory: &Path) -> Vec<PairingFile> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            if path
                .extension()
                .is_none_or(|extension| extension != "plist")
            {
                return None;
            }
            let udid = path.file_stem()?.to_str()?;
            if !matches!(udid.len(), 25 | 40)
                || !udid.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
            {
                return None;
            }
            let mut pairing = PairingFile::read_from_file(&path).ok()?;
            pairing.udid.get_or_insert_with(|| udid.to_owned());
            Some(pairing)
        })
        .collect()
}

async fn pairing_candidates() -> Vec<PairingFile> {
    let mut pairings = tokio::task::spawn_blocking(|| {
        pairing_directories()
            .iter()
            .flat_map(|path| read_pairings(path))
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();
    // On macOS, enumeration may be denied while usbmuxd can read known records.
    if let Ok(Ok(mut muxer)) = tokio::time::timeout(
        Duration::from_secs(2),
        UsbmuxdAddr::from_env_var().unwrap_or_default().connect(0),
    )
    .await
        && let Ok(Ok(devices)) =
            tokio::time::timeout(Duration::from_secs(2), muxer.get_devices()).await
    {
        for device in devices {
            if pairings
                .iter()
                .any(|pairing| pairing.udid.as_deref() == Some(&device.udid))
            {
                continue;
            }
            if let Ok(Ok(mut pairing)) =
                tokio::time::timeout(Duration::from_secs(2), muxer.get_pair_record(&device.udid))
                    .await
            {
                pairing.udid = Some(device.udid);
                pairings.push(pairing);
            }
        }
    }
    pairings
}

pub(crate) async fn connect_network_device(
    service: &str,
    addresses: &[SocketAddr],
    id: u32,
    route: Route,
) -> Result<(Device, HeartbeatClient, LockdownClient), Error> {
    let mut pairings = pairing_candidates().await;
    if pairings.is_empty() {
        return Err(Error::Other(
            "No saved pairing records; trust this computer over USB first".into(),
        ));
    }
    let advertised_mac = normalize_mac(service.split('@').next().unwrap_or_default());
    pairings.sort_by_key(|pairing| normalize_mac(&pairing.wifi_mac_address) != advertised_mac);
    let mut last_error = Error::Other("No reachable Wi-Fi addresses".into());
    for address in addresses {
        for pairing in &pairings {
            let provider = NetworkProvider {
                addr: address.ip(),
                scope_id: match address {
                    SocketAddr::V6(address) => Some(address.scope_id()),
                    _ => None,
                },
                pairing_file: pairing.clone(),
                label: CONNECTION_LABEL.to_owned(),
                route: route.clone(),
            };
            // Advertised MACs can differ from saved ones on newer iOS versions.
            // Try saved records, accepting a device only after authenticated TLS.
            match tokio::time::timeout(
                Duration::from_secs(5),
                initialize_network_device(provider, id),
            )
            .await
            {
                Ok(Ok(device)) => return Ok(device),
                Ok(Err(error)) => last_error = error,
                Err(_) => {
                    last_error = Error::Other(format!("Network connection timed out at {address}"))
                }
            }
        }
    }
    Err(last_error)
}

async fn initialize_network_device(
    provider: NetworkProvider,
    id: u32,
) -> Result<(Device, HeartbeatClient, LockdownClient), Error> {
    let mut lockdown = LockdownClient::connect(&provider).await?;
    lockdown.start_session(&provider.pairing_file).await?;
    let values = lockdown.get_value(None, None).await?;
    let value = |key| {
        values
            .as_dictionary()
            .and_then(|values| values.get(key))
            .and_then(plist::Value::as_string)
    };
    let udid = value("UniqueDeviceID")
        .filter(|udid| !udid.is_empty())
        .ok_or_else(|| Error::Other("Wi-Fi device did not return its UDID".into()))?;
    if provider
        .pairing_file
        .udid
        .as_deref()
        .is_some_and(|paired_udid| paired_udid != udid)
    {
        return Err(Error::Other(
            "Wi-Fi device does not match its pairing record".into(),
        ));
    }
    let name = value("DeviceName")
        .filter(|name| !name.is_empty())
        .unwrap_or(udid);
    log::info!(
        "Authenticated Wi-Fi device {name} ({})",
        value("ProductType").unwrap_or("unknown model")
    );
    let heartbeat = HeartbeatClient::connect(&provider).await?;
    Ok((
        Device {
            name: name.to_owned(),
            udid: udid.to_owned(),
            device_id: id,
            usbmuxd_device: None,
            network: Some(NetworkConnection::new(provider)),
            is_mac: false,
        },
        heartbeat,
        lockdown,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use idevice::Idevice;
    use plist::{Dictionary, Value};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use crate::network_connection::tests::fixture_pairing;

    fn fixture_device(id: u32, udid: &str) -> Device {
        Device {
            name: "Test iPhone".into(),
            udid: udid.into(),
            device_id: id,
            usbmuxd_device: None,
            is_mac: false,
            network: Some(NetworkConnection::new(NetworkProvider {
                label: CONNECTION_LABEL.into(),
                ..crate::network_connection::tests::provider("127.0.0.1")
            })),
        }
    }

    #[test]
    fn disconnected_wifi_device_expires_after_exactly_five_minutes() {
        let mut cache = WifiDeviceCache::default();
        let now = Instant::now();
        cache.connected(fixture_device(10, "phone"));
        assert!(cache.expire(now + Duration::from_secs(3600)).is_empty());
        cache.disconnected(10, now);
        assert!(cache.expire(now + Duration::from_secs(299)).is_empty());
        assert_eq!(cache.expire(now + Duration::from_secs(300)), vec![10]);
        assert!(cache.next_expiration().is_none());
        assert!(cache.connections.is_empty());
    }

    #[test]
    fn repeated_disconnects_do_not_extend_cache_and_devices_expire_independently() {
        let mut cache = WifiDeviceCache::default();
        let now = Instant::now();
        cache.connected(fixture_device(10, "phone"));
        cache.connected(fixture_device(11, "tablet"));
        cache.disconnected(10, now);
        cache.disconnected(11, now + Duration::from_secs(120));
        cache.disconnected(10, now + Duration::from_secs(240));
        assert_eq!(cache.expire(now + Duration::from_secs(300)), vec![10]);
        assert!(cache.expire(now + Duration::from_secs(419)).is_empty());
        assert_eq!(cache.expire(now + Duration::from_secs(420)), vec![11]);
    }

    #[test]
    fn rediscovery_keeps_device_id_and_cancels_expiration_even_with_new_connection_id() {
        let mut cache = WifiDeviceCache::default();
        let now = Instant::now();
        cache.connected(fixture_device(10, "phone"));
        cache.disconnected(10, now);
        let mut rediscovered = fixture_device(20, "phone");
        rediscovered.name = "Renamed iPhone".into();
        let device = cache.connected(rediscovered);
        assert_eq!(device.device_id, 10);
        assert_eq!(device.name, "Renamed iPhone");
        assert_eq!(cache.devices.len(), 1);
        assert!(cache.next_expiration().is_none());
        // The previous connection's late disconnect cannot evict the new one.
        cache.disconnected(10, now + Duration::from_secs(250));
        assert!(cache.expire(now + Duration::from_secs(600)).is_empty());
        cache.disconnected(20, now + Duration::from_secs(600));
        assert_eq!(cache.expire(now + Duration::from_secs(900)), vec![10]);
    }

    #[test]
    fn loads_pairings_without_embedded_udids_and_skips_unusable_records() {
        let directory = tempfile::tempdir().unwrap();
        let udid = "00008130-0123456789ABCDEF";
        std::fs::write(
            directory.path().join(format!("{udid}.plist")),
            fixture_pairing().serialize().unwrap(),
        )
        .unwrap();
        std::fs::write(
            directory.path().join("SystemConfiguration.plist"),
            b"not a pairing",
        )
        .unwrap();
        std::fs::write(
            directory.path().join("00008130-0123456789ABCDF0.plist"),
            b"malformed",
        )
        .unwrap();
        let pairings = read_pairings(directory.path());
        assert_eq!(pairings.len(), 1);
        assert_eq!(pairings[0].udid.as_deref(), Some(udid));
    }

    #[tokio::test]
    async fn cloned_network_device_routes_services_over_tcp_and_retains_pairing() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let device = Device {
            name: "Test iPhone".into(),
            udid: "test-device".into(),
            device_id: 0x8000_0000,
            usbmuxd_device: None,
            is_mac: false,
            network: Some(NetworkConnection::new(NetworkProvider {
                addr: address.ip(),
                label: "old label".into(),
                ..crate::network_connection::tests::provider("127.0.0.1")
            })),
        };
        let cloned = device.clone();
        let provider = cloned.provider("test service").unwrap();
        assert_eq!(provider.label(), "test service");
        assert_eq!(
            provider.get_pairing_file().await.unwrap().host_id,
            "fixture-host"
        );
        assert_eq!(cloned.to_string(), "[WiFi] Test iPhone");
        assert!(!format!("{cloned:?}").contains("fixture-private-key"));
        let (connection, accepted) =
            tokio::join!(provider.connect(address.port()), listener.accept());
        connection.unwrap();
        accepted.unwrap();
    }

    #[tokio::test]
    async fn heartbeat_answers_multiple_messages_and_detects_disconnect() {
        let (client, mut server) = tokio::io::duplex(4096);
        let mut heartbeat = HeartbeatClient::new(Idevice::new(Box::new(client), "test heartbeat"));
        let (result, ()) = tokio::join!(keep_alive(&mut heartbeat), async move {
            for _ in 0..2 {
                let mut marco = Dictionary::new();
                marco.insert("Interval".into(), 1.into());
                let mut bytes = Vec::new();
                Value::Dictionary(marco).to_writer_xml(&mut bytes).unwrap();
                server.write_u32(bytes.len() as u32).await.unwrap();
                server.write_all(&bytes).await.unwrap();
                let length = server.read_u32().await.unwrap();
                let mut bytes = vec![0; length as usize];
                server.read_exact(&mut bytes).await.unwrap();
                let response: Value = plist::from_bytes(&bytes).unwrap();
                assert_eq!(
                    response
                        .as_dictionary()
                        .unwrap()
                        .get("Command")
                        .and_then(Value::as_string),
                    Some("Polo")
                );
            }
        });
        assert!(result.is_err());
    }

    #[tokio::test]
    #[ignore = "requires a paired iPhone 15 Pro Max advertising on the local Wi-Fi network"]
    async fn live_wifi_iphone_15_pro_max() {
        let _ = env_logger::Builder::new()
            .filter_module("plume_utils", log::LevelFilter::Debug)
            .is_test(true)
            .try_init();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::join!(watch_network_devices(tx), async move {
            let device = tokio::time::timeout(Duration::from_secs(75), async {
                while let Some(event) = rx.recv().await {
                    if let DeviceEvent::Connected(device) = event {
                        let provider = device.provider(CONNECTION_LABEL).unwrap();
                        let pairing = provider.get_pairing_file().await.unwrap();
                        let mut lockdown =
                            LockdownClient::connect(provider.as_ref()).await.unwrap();
                        lockdown.start_session(&pairing).await.unwrap();
                        let model = lockdown.get_value(Some("ProductType"), None).await.unwrap();
                        println!(
                            "Discovered {device}, model {}",
                            model.as_string().unwrap_or_default()
                        );
                        if model.as_string() == Some("iPhone16,2") {
                            return device;
                        }
                    }
                }
                panic!("Wi-Fi discovery closed");
            })
            .await
            .expect("No paired iPhone 15 Pro Max discovered over Wi-Fi");

            tokio::time::timeout(Duration::from_secs(60), async {
                let worker_name = device
                    .network
                    .as_ref()
                    .unwrap()
                    .execute(CONNECTION_LABEL, |_| async {
                        Ok(std::thread::current().name().unwrap_or_default().to_owned())
                    })
                    .await
                    .unwrap();
                assert!(worker_name.starts_with("jimpactor-wifi-device-"));
                println!("Device operations run on {worker_name}");
                device
                    .installed_apps()
                    .await
                    .expect("Wi-Fi installation proxy must work");
                tokio::time::sleep(Duration::from_secs(20)).await;
                device
                    .installed_apps()
                    .await
                    .expect("Wi-Fi app access must stay alive");
                println!("Verified iPhone 15 Pro Max Wi-Fi app access");
            })
            .await
            .expect("Wi-Fi device operations timed out after discovery");
            drop(rx);
        });
    }
}
