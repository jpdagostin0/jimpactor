//! An embedded Tailscale node, so devices on the user's tailnet can be reached without
//! installing Tailscale on this computer. tailscale-rs cannot list peers yet, so devices
//! are found from the machine names or tailnet addresses the user enters.

use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::OnceLock;
#[cfg(tailnet)]
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

#[cfg(tailnet)]
use idevice::lockdown::LockdownClient;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::watch;

use crate::DeviceEvent;

/// What the embedded node is doing, for display in settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TailnetStatus {
    Off,
    /// This build cannot embed Tailscale (Intel Macs).
    Unsupported,
    Connecting,
    /// The user must approve this computer at the URL.
    LoginRequired(String),
    Connected { address: IpAddr },
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct TailnetConfig {
    pub hostname: String,
    pub control_url: Option<String>,
    /// Where this computer's node keys are kept between runs.
    pub key_file: PathBuf,
    /// An optional pre-authorized key; otherwise the user signs in through a URL.
    pub auth_key: Option<String>,
}

struct Shared {
    status: watch::Sender<TailnetStatus>,
    #[cfg(tailnet)]
    node: watch::Sender<Option<Arc<tailscale::Device>>>,
    /// Device names or addresses to look for on the tailnet.
    targets: watch::Sender<Vec<String>>,
    #[cfg(tailnet)]
    worker: Mutex<Option<NodeWorker>>,
}

fn shared() -> &'static Shared {
    static SHARED: OnceLock<Shared> = OnceLock::new();
    SHARED.get_or_init(|| Shared {
        status: watch::channel(if cfg!(tailnet) {
            TailnetStatus::Off
        } else {
            TailnetStatus::Unsupported
        })
        .0,
        #[cfg(tailnet)]
        node: watch::channel(None).0,
        targets: watch::channel(Vec::new()).0,
        #[cfg(tailnet)]
        worker: Mutex::new(None),
    })
}

pub fn tailnet_supported() -> bool {
    cfg!(tailnet)
}

pub fn tailnet_status() -> TailnetStatus {
    shared().status.borrow().clone()
}

pub fn subscribe_tailnet_status() -> watch::Receiver<TailnetStatus> {
    shared().status.subscribe()
}

/// Replaces the devices that tailnet discovery looks for.
pub fn set_tailnet_devices(targets: Vec<String>) {
    let targets: Vec<String> = targets
        .into_iter()
        .map(|target| target.trim().to_owned())
        .filter(|target| !target.is_empty())
        .collect();
    shared().targets.send_if_modified(|current| {
        let changed = *current != targets;
        *current = targets;
        changed
    });
}

/// Joins the tailnet, replacing any running node.
pub fn start_tailnet(config: TailnetConfig) {
    #[cfg(tailnet)]
    {
        let shared = shared();
        let mut worker = shared.worker.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(previous) = worker.take() {
            previous.stop();
        }
        shared.status.send_replace(TailnetStatus::Connecting);
        *worker = Some(NodeWorker::spawn(config));
    }
    #[cfg(not(tailnet))]
    let _ = config;
}

/// Leaves the tailnet. With `forget`, the node keys are deleted so the next start signs in again.
pub fn stop_tailnet(forget: Option<PathBuf>) {
    #[cfg(tailnet)]
    {
        let shared = shared();
        if let Some(worker) = shared
            .worker
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            worker.stop();
        }
        shared.node.send_replace(None);
        shared.status.send_replace(TailnetStatus::Off);
    }
    if let Some(key_file) = forget
        && let Err(error) = std::fs::remove_file(&key_file)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        log::warn!("Could not delete Tailscale keys at {key_file:?}: {error}");
    }
}

/// A TCP listener on this computer's tailnet address, for serving tailnet peers.
#[cfg(tailnet)]
pub async fn tailnet_listen(port: u16) -> Result<tailscale::netstack::TcpListener, crate::Error> {
    let node = shared()
        .node
        .borrow()
        .clone()
        .ok_or_else(|| crate::Error::Other("Tailscale is not connected".into()))?;
    let address = node
        .ipv4_addr()
        .await
        .map_err(|error| crate::Error::Other(error.to_string()))?;
    node.tcp_listen(SocketAddr::new(address.into(), port))
        .await
        .map_err(|error| crate::Error::Other(error.to_string()))
}

/// Owns the thread and runtime the node's actors live on.
#[cfg(tailnet)]
struct NodeWorker {
    stop: tokio::sync::oneshot::Sender<()>,
}

#[cfg(tailnet)]
impl NodeWorker {
    fn spawn(config: TailnetConfig) -> Self {
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let spawned = std::thread::Builder::new()
            .name("jimpactor-tailnet".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .thread_name("jimpactor-tailnet-io")
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        shared()
                            .status
                            .send_replace(TailnetStatus::Failed(error.to_string()));
                        return;
                    }
                };
                runtime.block_on(run_node(config, stopped));
            });
        if let Err(error) = spawned {
            shared()
                .status
                .send_replace(TailnetStatus::Failed(error.to_string()));
        }
        Self { stop }
    }

    fn stop(self) {
        let _ = self.stop.send(());
    }
}

#[cfg(tailnet)]
async fn run_node(config: TailnetConfig, mut stopped: tokio::sync::oneshot::Receiver<()>) {
    let shared = shared();
    let node = tokio::select! {
        _ = &mut stopped => return,
        node = start_node(&config) => node,
    };
    let node = match node {
        Ok(node) => Arc::new(node),
        Err(error) => {
            log::error!("Could not start Tailscale: {error}");
            shared.status.send_replace(TailnetStatus::Failed(error));
            return;
        }
    };

    let authorized = tokio::select! {
        _ = &mut stopped => false,
        address = wait_until_authorized(&node) => match address {
            Ok(address) => {
                log::info!("Joined the tailnet as {address}");
                shared.status.send_replace(TailnetStatus::Connected { address });
                shared.node.send_replace(Some(node.clone()));
                true
            }
            Err(error) => {
                log::error!("Tailscale authorization failed: {error}");
                shared.status.send_replace(TailnetStatus::Failed(error));
                false
            }
        },
    };
    if authorized {
        let _ = stopped.await;
    }

    // Discovery holds clones of the node; release ours and let the runtime end its actors.
    shared.node.send_if_modified(|current| {
        let ours = current.as_ref().is_some_and(|c| Arc::ptr_eq(c, &node));
        if ours {
            *current = None;
        }
        ours
    });
    match Arc::try_unwrap(node) {
        Ok(node) => {
            node.shutdown(Some(Duration::from_secs(3))).await;
        }
        Err(_) => log::debug!("Tailscale node still in use while stopping"),
    }
}

#[cfg(tailnet)]
async fn start_node(config: &TailnetConfig) -> Result<tailscale::Device, String> {
    if let Some(parent) = config.key_file.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| e.to_string())?;
    }
    let mut node_config = tailscale::Config::default_with_key_file(&config.key_file)
        .await
        .map_err(|e| e.to_string())?;
    node_config.client_name = Some("Jimpactor".into());
    let hostname = config.hostname.trim();
    if !hostname.is_empty() {
        node_config.requested_hostname = Some(hostname.to_owned());
    }
    if let Some(url) = config.control_url.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
        node_config.control_server_url = url
            .parse()
            .map_err(|e| format!("Invalid coordination server URL {url}: {e}"))?;
    }
    let auth_key = config
        .auth_key
        .clone()
        .filter(|key| !key.trim().is_empty());
    tailscale::Device::new(&node_config, auth_key)
        .await
        .map_err(|e| e.to_string())
}

#[cfg(tailnet)]
async fn wait_until_authorized(node: &tailscale::Device) -> Result<IpAddr, String> {
    loop {
        match node.is_authorized().await.map_err(|e| e.to_string())? {
            tailscale::AuthState::Authorized => {
                return node
                    .ipv4_addr()
                    .await
                    .map(IpAddr::from)
                    .map_err(|e| e.to_string());
            }
            tailscale::AuthState::NotAuthorized(url) => {
                shared()
                    .status
                    .send_replace(TailnetStatus::LoginRequired(url.to_string()));
            }
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

/// Watches the configured tailnet devices while the node is connected. Devices are
/// accepted only after a saved pairing record authenticates, as on Wi-Fi.
pub async fn watch_tailnet_devices(output: UnboundedSender<DeviceEvent>) {
    #[cfg(tailnet)]
    {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::join!(
            watch_targets(tx),
            crate::network_discovery::forward_cached_devices(rx, output)
        );
    }
    #[cfg(not(tailnet))]
    output.closed().await;
}

#[cfg(tailnet)]
async fn watch_targets(output: UnboundedSender<DeviceEvent>) {
    use crate::device_worker::DeviceWorker;
    use crate::network_connection::NetworkConnections;

    let shared = shared();
    let mut node_rx = shared.node.subscribe();
    let mut targets_rx = shared.targets.subscribe();
    let connections = NetworkConnections::default();
    // IDs above the Wi-Fi range keep tailnet entries distinct from LAN ones.
    let mut next_id = 0xC000_0000u32;
    let mut workers: HashMap<String, (u32, DeviceWorker)> = HashMap::new();
    let mut current_node: Option<Arc<tailscale::Device>> = None;

    loop {
        let node = node_rx.borrow_and_update().clone();
        let targets = targets_rx.borrow_and_update().clone();
        let node_changed = match (&node, &current_node) {
            (Some(a), Some(b)) => !Arc::ptr_eq(a, b),
            (None, None) => false,
            _ => true,
        };
        if node_changed {
            for (_, (id, worker)) in workers.drain() {
                worker.shutdown().await;
                let _ = output.send(DeviceEvent::Disconnected(id));
            }
            current_node = node;
        }

        let stale: Vec<String> = workers
            .keys()
            .filter(|target| !targets.contains(target))
            .cloned()
            .collect();
        for target in stale {
            if let Some((id, worker)) = workers.remove(&target) {
                worker.shutdown().await;
                let _ = output.send(DeviceEvent::Disconnected(id));
            }
        }

        if let Some(node) = &current_node {
            for target in targets {
                if workers.contains_key(&target) {
                    continue;
                }
                let id = next_id;
                next_id = next_id.checked_add(1).filter(|id| *id != u32::MAX).unwrap_or(0xC000_0000);
                let node = node.clone();
                let output = output.clone();
                let connections = connections.clone();
                let name = target.clone();
                match DeviceWorker::spawn(format!("jimpactor-tailnet-device-{id}"), move || {
                    run_tailnet_device(name, node, id, output, connections)
                }) {
                    Ok(worker) => {
                        workers.insert(target, (id, worker));
                    }
                    Err(error) => log::error!("Could not watch tailnet device {target}: {error}"),
                }
            }
        }

        tokio::select! {
            _ = output.closed() => break,
            changed = node_rx.changed() => if changed.is_err() { break },
            changed = targets_rx.changed() => if changed.is_err() { break },
        }
    }
    for (_, (_, worker)) in workers.drain() {
        worker.shutdown().await;
    }
}

/// Resolves a configured target to a tailnet address: a literal IP, or a machine name
/// (full `name.tailnet.ts.net` or the short name).
#[cfg(tailnet)]
async fn resolve_target(node: &tailscale::Device, target: &str) -> Result<IpAddr, String> {
    if let Ok(address) = target.parse::<IpAddr>() {
        return Ok(address);
    }
    let candidates = [target, target.split('.').next().unwrap_or(target)];
    for name in candidates {
        let peer = tokio::time::timeout(Duration::from_secs(10), node.peer_by_name(name))
            .await
            .map_err(|_| format!("Timed out looking up {target} on the tailnet"))?
            .map_err(|e| e.to_string())?;
        if let Some(peer) = peer {
            return Ok(IpAddr::V4(peer.tailnet_address.ipv4.addr()));
        }
    }
    Err(format!("No tailnet machine named {target}"))
}

#[cfg(tailnet)]
async fn run_tailnet_device(
    target: String,
    node: Arc<tailscale::Device>,
    id: u32,
    output: UnboundedSender<DeviceEvent>,
    connections: crate::network_connection::NetworkConnections,
) {
    use crate::device::CONNECTION_LABEL;
    use crate::network_connection::Route;
    use crate::network_discovery::{connect_network_device, keep_alive};

    loop {
        let attempt = async {
            let address = resolve_target(&node, &target).await?;
            connect_network_device(
                &target,
                &[SocketAddr::new(address, LockdownClient::LOCKDOWND_PORT)],
                id,
                Route::Tailnet(node.clone()),
            )
            .await
            .map_err(|e| e.to_string())
        };
        match attempt.await {
            Ok((mut device, mut heartbeat, lockdown)) => {
                let provider = device
                    .network
                    .as_ref()
                    .expect("initialized network transport")
                    .provider(CONNECTION_LABEL);
                let (connection, session) = connections.connected(&device.udid, provider);
                device.network = Some(connection);
                let _lockdown = lockdown;
                log::info!("Tailscale device connected: {device}");
                if output.send(DeviceEvent::Connected(device)).is_err() {
                    return;
                }
                let result = keep_alive(&mut heartbeat).await;
                drop(session);
                log::debug!("Tailscale connection to {target} ended: {result:?}");
                if output.send(DeviceEvent::Disconnected(id)).is_err() {
                    return;
                }
            }
            Err(error) => log::debug!("Tailscale device {target} is not ready: {error}"),
        }
        tokio::select! {
            _ = output.closed() => return,
            _ = tokio::time::sleep(Duration::from_secs(10)) => {}
        }
    }
}
