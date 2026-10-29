//! A small HTTP server that enables JIT for apps on connected devices, so a device can
//! request JIT for itself (for example from a Shortcut) over Wi-Fi or Tailscale.
//!
//! - `GET /` lists connected devices.
//! - `GET /{udid}` lists the device's installed apps.
//! - `GET /{udid}/{bundle_id}` launches the app with JIT enabled.
//! - `GET /{udid}/attach/{pid}` enables JIT for a running process.

use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use plume_store::{JitListen, JitServerSettings};
use serde_json::json;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{oneshot, watch};

use crate::refresh::ConnectedDevices;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum JitServerStatus {
    Off,
    Starting,
    Listening(String),
    Failed(String),
}

struct Server {
    status: watch::Sender<JitServerStatus>,
    stop: Mutex<Option<oneshot::Sender<()>>>,
}

fn server() -> &'static Server {
    static SERVER: OnceLock<Server> = OnceLock::new();
    SERVER.get_or_init(|| Server {
        status: watch::channel(JitServerStatus::Off).0,
        stop: Mutex::new(None),
    })
}

pub(crate) fn status() -> JitServerStatus {
    server().status.borrow().clone()
}

/// Starts, restarts or stops the server to match `settings`.
pub(crate) fn apply(settings: &JitServerSettings, devices: ConnectedDevices) {
    let server = server();
    if let Some(stop) = server.stop.lock().unwrap_or_else(|e| e.into_inner()).take() {
        let _ = stop.send(());
    }
    if !settings.enabled {
        server.status.send_replace(JitServerStatus::Off);
        return;
    }
    server.status.send_replace(JitServerStatus::Starting);
    let (stop, stopped) = oneshot::channel();
    *server.stop.lock().unwrap_or_else(|e| e.into_inner()) = Some(stop);
    let settings = settings.clone();
    let spawned = std::thread::Builder::new()
        .name("jimpactor-jit-server".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    server.status.send_replace(JitServerStatus::Failed(error.to_string()));
                    return;
                }
            };
            runtime.block_on(async move {
                tokio::select! {
                    _ = stopped => {}
                    _ = serve(settings, devices) => {}
                }
            });
        });
    if let Err(error) = spawned {
        server.status.send_replace(JitServerStatus::Failed(error.to_string()));
    }
}

async fn serve(settings: JitServerSettings, devices: ConnectedDevices) {
    let status = &server().status;
    match settings.listen {
        JitListen::Local | JitListen::Network => {
            let host = if settings.listen == JitListen::Local {
                "127.0.0.1"
            } else {
                "0.0.0.0"
            };
            let listener = match tokio::net::TcpListener::bind((host, settings.port)).await {
                Ok(listener) => listener,
                Err(error) => {
                    log::error!("JIT server could not listen on port {}: {error}", settings.port);
                    status.send_replace(JitServerStatus::Failed(format!(
                        "Could not listen on port {}: {error}",
                        settings.port
                    )));
                    return;
                }
            };
            status.send_replace(JitServerStatus::Listening(format!("{host}:{}", settings.port)));
            log::info!("JIT server listening on {host}:{}", settings.port);
            loop {
                match listener.accept().await {
                    Ok((stream, peer)) => {
                        log::debug!("JIT request from {peer}");
                        tokio::spawn(handle(stream, devices.clone()));
                    }
                    Err(error) => log::warn!("JIT server accept failed: {error}"),
                }
            }
        }
        JitListen::Tailnet => serve_tailnet(settings.port, devices).await,
    }
}

#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
async fn serve_tailnet(port: u16, devices: ConnectedDevices) {
    let status = &server().status;
    loop {
        match plume_utils::tailnet_listen(port).await {
            Ok(listener) => {
                let address = match plume_utils::tailnet_status() {
                    plume_utils::TailnetStatus::Connected { address } => format!("{address}:{port}"),
                    _ => format!("tailnet port {port}"),
                };
                log::info!("JIT server listening on {address}");
                status.send_replace(JitServerStatus::Listening(address));
                loop {
                    match listener.accept().await {
                        Ok(stream) => {
                            tokio::spawn(handle(stream, devices.clone()));
                        }
                        Err(error) => {
                            log::warn!("Tailnet JIT listener stopped: {error}");
                            break;
                        }
                    }
                }
            }
            Err(error) => {
                status.send_replace(JitServerStatus::Failed(format!(
                    "{error}. Waiting for Tailscale to connect…"
                )));
            }
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
async fn serve_tailnet(_port: u16, _devices: ConnectedDevices) {
    server().status.send_replace(JitServerStatus::Failed(
        "Tailscale is not supported on Intel Macs".into(),
    ));
}

async fn handle<S: AsyncRead + AsyncWrite + Unpin>(mut stream: S, devices: ConnectedDevices) {
    let (status, body) = match read_request_path(&mut stream).await {
        Ok(path) => route(&path, devices).await,
        Err(error) => (400, json!({ "ok": false, "error": error })),
    };
    let body = body.to_string();
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Internal Server Error",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// Reads the request head and returns the path of a `GET` request.
async fn read_request_path<S: AsyncRead + Unpin>(stream: &mut S) -> Result<String, String> {
    let mut head = Vec::new();
    let mut buffer = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        if head.len() > 16 * 1024 {
            return Err("Request is too large".into());
        }
        let read = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut buffer))
            .await
            .map_err(|_| "Request timed out".to_string())?
            .map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        head.extend_from_slice(&buffer[..read]);
    }
    let head = String::from_utf8_lossy(&head);
    let mut request_line = head.lines().next().unwrap_or_default().split_whitespace();
    match (request_line.next(), request_line.next()) {
        (Some("GET"), Some(target)) => Ok(target.split('?').next().unwrap_or_default().to_owned()),
        (Some(_), Some(_)) => Err("Only GET requests are supported".into()),
        _ => Err("Malformed request".into()),
    }
}

async fn route(path: &str, devices: ConnectedDevices) -> (u16, serde_json::Value) {
    let segments: Vec<String> = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(percent_decode)
        .collect();
    let Some(udid) = segments.first() else {
        let list: Vec<_> = connected(&devices)
            .iter()
            .map(|device| json!({ "udid": device.udid, "name": device.name, "connection": device.to_string() }))
            .collect();
        return (200, json!({ "ok": true, "devices": list }));
    };
    let Some(device) = connected(&devices)
        .into_iter()
        .find(|device| device.udid.eq_ignore_ascii_case(udid))
    else {
        return (404, json!({ "ok": false, "error": format!("Device {udid} is not connected") }));
    };
    let ddi_dir = crate::defaults::get_data_path().join("ddi");

    match segments.as_slice() {
        [_] => match device.installed_apps().await {
            Ok(apps) => {
                let apps: Vec<_> = apps
                    .iter()
                    .map(|app| json!({
                        "name": app.installed_name.clone().unwrap_or_else(|| app.app.to_string()),
                        "bundle_id": app.bundle_id,
                    }))
                    .collect();
                (200, json!({ "ok": true, "apps": apps }))
            }
            Err(error) => (500, json!({ "ok": false, "error": error.to_string() })),
        },
        [_, attach, pid] if attach == "attach" => {
            let Ok(pid) = pid.parse::<u64>() else {
                return (400, json!({ "ok": false, "error": "Invalid process ID" }));
            };
            jit_result(device.attach_jit(pid, ddi_dir).await, &device.name)
        }
        [_, bundle_id] => {
            log::info!("Enabling JIT for {bundle_id} on {}", device.name);
            jit_result(device.launch_with_jit(bundle_id, ddi_dir).await, &device.name)
        }
        _ => (404, json!({ "ok": false, "error": "Unknown endpoint" })),
    }
}

fn jit_result(result: Result<u64, plume_utils::Error>, device: &str) -> (u16, serde_json::Value) {
    match result {
        Ok(pid) => {
            log::info!("Enabled JIT for process {pid} on {device}");
            (200, json!({ "ok": true, "pid": pid }))
        }
        Err(error) => {
            log::error!("Could not enable JIT on {device}: {error}");
            (500, json!({ "ok": false, "error": error.to_string() }))
        }
    }
}

/// Available devices, preferring USB connections, one entry per UDID.
fn connected(devices: &ConnectedDevices) -> Vec<plume_utils::Device> {
    let mut list: Vec<_> = devices
        .lock()
        .map(|devices| devices.values().filter(|d| d.is_available() && !d.is_mac).cloned().collect())
        .unwrap_or_default();
    list.sort_by(|a, b| a.name.cmp(&b.name));
    list
}

fn percent_decode(segment: &str) -> String {
    let bytes = segment.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(byte) = segment
                .get(i + 1..i + 3)
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
        {
            decoded.push(byte);
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn parses_get_paths_and_rejects_other_methods() {
        let mut request: &[u8] = b"GET /abc/com.example.app?x=1 HTTP/1.1\r\nHost: x\r\n\r\n";
        assert_eq!(
            read_request_path(&mut request).await.unwrap(),
            "/abc/com.example.app"
        );
        let mut post: &[u8] = b"POST / HTTP/1.1\r\n\r\n";
        assert!(read_request_path(&mut post).await.is_err());
    }

    #[test]
    fn decodes_percent_escapes() {
        assert_eq!(percent_decode("My%20App"), "My App");
        assert_eq!(percent_decode("bad%zz"), "bad%zz");
    }

    #[tokio::test]
    async fn lists_no_devices_and_reports_unknown_ones() {
        let devices = ConnectedDevices::default();
        let (status, body) = route("/", devices.clone()).await;
        assert_eq!(status, 200);
        assert_eq!(body["devices"].as_array().unwrap().len(), 0);
        let (status, _) = route("/00008130-0123/com.example.app", devices).await;
        assert_eq!(status, 404);
    }
}
