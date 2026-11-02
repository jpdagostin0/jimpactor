//! A small HTTP server that enables JIT for apps on connected devices, so a device can
//! request JIT for itself (for example from a Shortcut) over Wi-Fi or Tailscale.
//!
//! - `GET /` lists connected devices.
//! - `GET /{udid}` lists the device's installed apps.
//! - `GET /{udid}/{bundle_id}` launches the app with JIT enabled.
//! - `GET /{udid}/attach/{pid}` enables JIT for a running process.
//!
//! With a password set, every request must carry it: `?password=...`, which a Shortcut can
//! do easily, or an `Authorization: Bearer ...` header. It crosses the network unencrypted.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use plume_store::{JitListen, JitPassword, JitServerSettings};
use serde_json::json;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{oneshot, watch};

use crate::Device;

/// Devices the server can reach, by UDID.
pub type ConnectedDevices = Arc<std::sync::Mutex<HashMap<String, Device>>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JitServerStatus {
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

pub fn status() -> JitServerStatus {
    server().status.borrow().clone()
}

/// Starts, restarts or stops the server to match `settings`.
pub fn apply(settings: &JitServerSettings, devices: ConnectedDevices, ddi_dir: PathBuf) {
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
                    server
                        .status
                        .send_replace(JitServerStatus::Failed(error.to_string()));
                    return;
                }
            };
            runtime.block_on(async move {
                tokio::select! {
                    _ = stopped => {}
                    _ = serve(settings, devices, ddi_dir) => {}
                }
            });
        });
    if let Err(error) = spawned {
        server
            .status
            .send_replace(JitServerStatus::Failed(error.to_string()));
    }
}

/// Slows password guessing; each wrong one costs an attacker this long.
const WRONG_PASSWORD_DELAY: Duration = Duration::from_secs(1);

async fn serve(settings: JitServerSettings, devices: ConnectedDevices, ddi_dir: PathBuf) {
    let status = &server().status;
    let password = settings.password.clone();
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
                    log::error!(
                        "JIT server could not listen on port {}: {error}",
                        settings.port
                    );
                    status.send_replace(JitServerStatus::Failed(format!(
                        "Could not listen on port {}: {error}",
                        settings.port
                    )));
                    return;
                }
            };
            status.send_replace(JitServerStatus::Listening(format!(
                "{host}:{}",
                settings.port
            )));
            log::info!("JIT server listening on {host}:{}", settings.port);
            loop {
                match listener.accept().await {
                    Ok((stream, peer)) => {
                        log::debug!("JIT request from {peer}");
                        tokio::spawn(handle(
                            stream,
                            devices.clone(),
                            ddi_dir.clone(),
                            password.clone(),
                        ));
                    }
                    Err(error) => log::warn!("JIT server accept failed: {error}"),
                }
            }
        }
        JitListen::Tailnet => serve_tailnet(settings.port, devices, ddi_dir, password).await,
    }
}

#[cfg(tailnet)]
async fn serve_tailnet(
    port: u16,
    devices: ConnectedDevices,
    ddi_dir: PathBuf,
    password: Option<JitPassword>,
) {
    let status = &server().status;
    loop {
        match crate::tailnet_listen(port).await {
            Ok(listener) => {
                let address = match crate::tailnet_status() {
                    crate::TailnetStatus::Connected { address } => {
                        format!("{address}:{port}")
                    }
                    _ => format!("tailnet port {port}"),
                };
                log::info!("JIT server listening on {address}");
                status.send_replace(JitServerStatus::Listening(address));
                loop {
                    match listener.accept().await {
                        Ok(stream) => {
                            tokio::spawn(handle(
                                stream,
                                devices.clone(),
                                ddi_dir.clone(),
                                password.clone(),
                            ));
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

#[cfg(not(tailnet))]
async fn serve_tailnet(
    _port: u16,
    _devices: ConnectedDevices,
    _ddi_dir: PathBuf,
    _password: Option<JitPassword>,
) {
    server().status.send_replace(JitServerStatus::Failed(
        "Tailscale is not supported on Intel Macs".into(),
    ));
}

async fn handle<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    devices: ConnectedDevices,
    ddi_dir: PathBuf,
    password: Option<JitPassword>,
) {
    let (status, body) = match read_request(&mut stream).await {
        Ok(request) if !authorized(password.as_ref(), request.password.as_deref()) => {
            log::warn!("JIT request with a wrong or missing password");
            tokio::time::sleep(WRONG_PASSWORD_DELAY).await;
            (
                401,
                json!({ "ok": false, "error": "Wrong or missing password" }),
            )
        }
        Ok(request) => route(&request.path, devices, ddi_dir).await,
        Err(error) => (400, json!({ "ok": false, "error": error })),
    };
    let body = body.to_string();
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
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

fn authorized(expected: Option<&JitPassword>, given: Option<&str>) -> bool {
    expected.is_none_or(|expected| given.is_some_and(|given| expected.matches(given)))
}

#[derive(Debug, PartialEq, Eq)]
struct Request {
    path: String,
    password: Option<String>,
}

/// The password in `?password=...`, or else in an `Authorization: Bearer` header.
fn request_password(head: &str, query: Option<&str>) -> Option<String> {
    query
        .into_iter()
        .flat_map(|query| query.split('&'))
        .find_map(|pair| pair.strip_prefix("password="))
        .map(|value| percent_decode(&value.replace('+', " ")))
        .or_else(|| {
            head.lines().skip(1).find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.trim()
                    .eq_ignore_ascii_case("authorization")
                    .then(|| value.trim().strip_prefix("Bearer "))?
                    .map(|token| token.trim().to_owned())
            })
        })
}

/// Reads the head of a `GET` request.
async fn read_request<S: AsyncRead + Unpin>(stream: &mut S) -> Result<Request, String> {
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
        (Some("GET"), Some(target)) => {
            let (path, query) = match target.split_once('?') {
                Some((path, query)) => (path, Some(query)),
                None => (target, None),
            };
            Ok(Request {
                path: path.to_owned(),
                password: request_password(&head, query),
            })
        }
        (Some(_), Some(_)) => Err("Only GET requests are supported".into()),
        _ => Err("Malformed request".into()),
    }
}

async fn route(
    path: &str,
    devices: ConnectedDevices,
    ddi_dir: PathBuf,
) -> (u16, serde_json::Value) {
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
        return (
            404,
            json!({ "ok": false, "error": format!("Device {udid} is not connected") }),
        );
    };

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
            jit_result(
                device.launch_with_jit(bundle_id, ddi_dir).await,
                &device.name,
            )
        }
        _ => (404, json!({ "ok": false, "error": "Unknown endpoint" })),
    }
}

fn jit_result(result: Result<u64, crate::Error>, device: &str) -> (u16, serde_json::Value) {
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
fn connected(devices: &ConnectedDevices) -> Vec<crate::Device> {
    let mut list: Vec<_> = devices
        .lock()
        .map(|devices| {
            devices
                .values()
                .filter(|d| d.is_available() && !d.is_mac)
                .cloned()
                .collect()
        })
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
            read_request(&mut request).await.unwrap().path,
            "/abc/com.example.app"
        );
        let mut post: &[u8] = b"POST / HTTP/1.1\r\n\r\n";
        assert!(read_request(&mut post).await.is_err());
    }

    #[tokio::test]
    async fn passwords_come_from_the_query_or_a_bearer_header() {
        let mut request: &[u8] = b"GET /a?x=1&password=my%20secret HTTP/1.1\r\n\r\n";
        let request = read_request(&mut request).await.unwrap();
        assert_eq!(request.path, "/a");
        assert_eq!(request.password.as_deref(), Some("my secret"));

        let mut request: &[u8] =
            b"GET /a HTTP/1.1\r\nHost: x\r\nauthorization: Bearer token\r\n\r\n";
        let request = read_request(&mut request).await.unwrap();
        assert_eq!(request.password.as_deref(), Some("token"));

        let mut request: &[u8] = b"GET /a HTTP/1.1\r\nAuthorization: Basic abc\r\n\r\n";
        assert_eq!(read_request(&mut request).await.unwrap().password, None);
    }

    #[test]
    fn requests_need_the_password_only_when_one_is_set() {
        let password = JitPassword::new("secret");
        assert!(authorized(None, None));
        assert!(authorized(None, Some("anything")));
        assert!(authorized(Some(&password), Some("secret")));
        assert!(!authorized(Some(&password), Some("wrong")));
        assert!(!authorized(Some(&password), None));
    }

    #[test]
    fn decodes_percent_escapes() {
        assert_eq!(percent_decode("My%20App"), "My App");
        assert_eq!(percent_decode("bad%zz"), "bad%zz");
    }

    #[tokio::test]
    async fn lists_no_devices_and_reports_unknown_ones() {
        let devices = ConnectedDevices::default();
        let (status, body) = route("/", devices.clone(), PathBuf::new()).await;
        assert_eq!(status, 200);
        assert_eq!(body["devices"].as_array().unwrap().len(), 0);
        let (status, _) = route("/00008130-0123/com.example.app", devices, PathBuf::new()).await;
        assert_eq!(status, 404);
    }
}
