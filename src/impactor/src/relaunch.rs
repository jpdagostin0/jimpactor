#![cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]

use plume_utils::ipc::{self, Event, Request};
use serde::{Deserialize, Serialize};
use single_instance::SingleInstance;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
const SINGLE_INSTANCE_FILE: &str = "single-instance.lock";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RelaunchAction {
    Show,
    Restart,
}

/// Answers a JSON IPC request, calling `emit` for every event it sends back. Runs on the
/// connection's own thread and may block until the request is finished.
pub(crate) type RequestHandler = Arc<dyn Fn(Request, &mut dyn FnMut(Event)) + Send + Sync>;

#[derive(Debug, Serialize, Deserialize)]
struct InstanceInfo {
    version: String,
    executable: PathBuf,
}

fn relaunch_port_path() -> PathBuf {
    crate::defaults::get_data_path().join(ipc::PORT_FILE)
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
pub(crate) fn single_instance_key() -> String {
    crate::APP_NAME.to_owned()
}
#[cfg(target_os = "macos")]
pub(crate) fn single_instance_key() -> String {
    crate::defaults::get_data_path()
        .join(SINGLE_INSTANCE_FILE)
        .to_string_lossy()
        .into_owned()
}

fn send_command(port: u16, command: &[u8]) -> Result<Vec<u8>, String> {
    let mut stream =
        TcpStream::connect_timeout(&(Ipv4Addr::LOCALHOST, port).into(), Duration::from_secs(2))
            .map_err(|e| format!("connect: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    stream
        .write_all(command)
        .map_err(|e| format!("send: {e}"))?;
    stream
        .shutdown(Shutdown::Write)
        .map_err(|e| e.to_string())?;
    let mut reply = Vec::new();
    stream
        .take(8192)
        .read_to_end(&mut reply)
        .map_err(|e| format!("read: {e}"))?;
    Ok(reply)
}

fn version_choice_required(info: Option<&InstanceInfo>, launched: &str) -> bool {
    info.is_none_or(|info| info.version != launched)
}

fn wait_for_instance(key: &str, timeout: Duration) -> Result<SingleInstance, String> {
    let deadline = Instant::now() + timeout;
    loop {
        let instance = SingleInstance::new(key).map_err(|e| e.to_string())?;
        if instance.is_single() {
            return Ok(instance);
        }
        drop(instance);
        if Instant::now() >= deadline {
            return Err("The running app did not exit. Finish any installation or refresh, quit Jimpactor from its tray menu, and launch the new executable again.".into());
        }
        std::thread::sleep(Duration::from_millis(100).min(timeout));
    }
}

/// Returns None when this launch was cancelled or delegated to the running app.
pub(crate) fn acquire_or_notify() -> Result<Option<SingleInstance>, String> {
    let key = single_instance_key();
    let instance = SingleInstance::new(&key).map_err(|e| e.to_string())?;
    if instance.is_single() {
        return Ok(Some(instance));
    }
    drop(instance);
    let port: u16 = std::fs::read_to_string(relaunch_port_path())
        .map_err(|e| format!("Read running app endpoint: {e}"))?
        .trim()
        .parse()
        .map_err(|e| format!("Read running app port: {e}"))?;
    let info = send_command(port, b"info")
        .ok()
        .and_then(|bytes| serde_json::from_slice::<InstanceInfo>(&bytes).ok());
    let launched = env!("CARGO_PKG_VERSION");
    if !version_choice_required(info.as_ref(), launched) {
        send_command(port, b"show")?;
        return Ok(None);
    }
    let Some(info) = info else {
        let result = rfd::MessageDialog::new().set_title("A Jimpactor build is already running")
            .set_description(format!("Running version: unavailable (an older build).\nNewly launched version: {launched}.\n\nChoose OK to use the running build. To use the new executable, choose Cancel, quit the running app from its tray menu, then open the new executable again."))
            .set_buttons(rfd::MessageButtons::OkCancel).show();
        if result == rfd::MessageDialogResult::Ok {
            send_command(port, b"show")?;
        }
        return Ok(None);
    };
    let result = rfd::MessageDialog::new().set_title("Choose the Jimpactor version to use")
        .set_description(format!("Running version: {}\n{}\n\nNewly launched version: {launched}\n{}\n\nYes: restart into {launched}.\nNo: continue using {}.\nCancel: close this launch.\n\nFinish active installations and refreshes before restarting.", info.version, info.executable.display(), std::env::current_exe().map_err(|e| e.to_string())?.display(), info.version))
        .set_buttons(rfd::MessageButtons::YesNoCancel).show();
    match result {
        rfd::MessageDialogResult::Yes => {
            send_command(port, b"restart")?;
            wait_for_instance(&key, Duration::from_secs(10)).map(Some)
        }
        rfd::MessageDialogResult::No => {
            send_command(port, b"show")?;
            Ok(None)
        }
        _ => Ok(None),
    }
}

fn handle_connection(
    mut stream: TcpStream,
    info: &InstanceInfo,
    token: &str,
    action: impl Fn(RelaunchAction),
    on_request: &RequestHandler,
) -> Result<(), String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    let command = ipc::read_request(&mut stream).map_err(|e| e.to_string())?;
    match command.as_slice() {
        b"info" => stream
            .write_all(&serde_json::to_vec(info).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string()),
        b"show" | b"restart" => {
            stream.write_all(b"ok").map_err(|e| e.to_string())?;
            action(if command == b"show" {
                RelaunchAction::Show
            } else {
                RelaunchAction::Restart
            });
            Ok(())
        }
        json if json.trim_ascii_start().starts_with(b"{") => {
            let request = match ipc::parse_request(json, token) {
                Ok(request) => request,
                Err(error) => {
                    let _ = ipc::write_event(&mut stream, &Event::error(error.clone()));
                    return Err(error);
                }
            };
            // Keep running the request after the client goes away; its events are dropped.
            let mut connected = true;
            on_request(request, &mut |event| {
                if connected && ipc::write_event(&mut stream, &event).is_err() {
                    log::info!("IPC client disconnected; continuing without it");
                    connected = false;
                }
            });
            Ok(())
        }
        _ => Err("Unsupported relaunch command".into()),
    }
}

pub(crate) fn start_listener<F>(on_action: F, on_request: RequestHandler) -> Result<(), String>
where
    F: Fn(RelaunchAction) + Send + Sync + 'static,
{
    let listener =
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| format!("bind listener: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let info = Arc::new(InstanceInfo {
        version: env!("CARGO_PKG_VERSION").into(),
        executable: std::env::current_exe().map_err(|e| e.to_string())?,
    });
    // The token goes first so clients never pair a new port with a stale token.
    let token: Arc<str> = ipc::write_token(&crate::defaults::get_data_path())
        .map_err(|e| format!("write IPC token: {e}"))?
        .into();
    std::fs::write(relaunch_port_path(), port.to_string())
        .map_err(|e| format!("write port: {e}"))?;
    let on_action = Arc::new(on_action);
    std::thread::spawn(move || {
        for incoming in listener.incoming() {
            match incoming {
                Ok(stream) => {
                    // Installs take minutes, so each connection gets its own thread.
                    let (info, token, on_action, on_request) = (
                        info.clone(),
                        token.clone(),
                        on_action.clone(),
                        on_request.clone(),
                    );
                    std::thread::spawn(move || {
                        if let Err(error) =
                            handle_connection(stream, &info, &token, &*on_action, &on_request)
                        {
                            log::warn!("Failed to handle IPC request: {error}");
                        }
                    });
                }
                Err(error) => {
                    log::warn!("Relaunch listener stopped: {error}");
                    break;
                }
            }
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_requests() -> RequestHandler {
        Arc::new(|_, emit| emit(Event::error("unexpected")))
    }

    #[test]
    fn version_query_and_relaunch_actions_are_distinct() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let info = InstanceInfo {
                version: "0.1.3".into(),
                executable: "older-jimpactor".into(),
            };
            for _ in 0..3 {
                handle_connection(
                    listener.accept().unwrap().0,
                    &info,
                    "token",
                    |action| tx.send(action).unwrap(),
                    &no_requests(),
                )
                .unwrap();
            }
        });
        let info: InstanceInfo =
            serde_json::from_slice(&send_command(port, b"info").unwrap()).unwrap();
        assert_eq!(info.version, "0.1.3");
        assert!(!version_choice_required(Some(&info), "0.1.3"));
        assert!(version_choice_required(Some(&info), "0.1.4"));
        assert!(version_choice_required(None, "0.1.4"));
        send_command(port, b"show").unwrap();
        send_command(port, b"restart").unwrap();
        server.join().unwrap();
        assert_eq!(
            rx.try_iter().collect::<Vec<_>>(),
            vec![RelaunchAction::Show, RelaunchAction::Restart]
        );
    }
    #[test]
    fn json_requests_are_authenticated_and_streamed() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let handler: RequestHandler = Arc::new(|request, emit| {
            assert_eq!(request, Request::ListDevices);
            emit(Event::Progress {
                message: "looking".into(),
                percent: 50,
            });
            emit(Event::Devices { devices: vec![] });
        });
        let server = std::thread::spawn(move || {
            let info = InstanceInfo {
                version: "0".into(),
                executable: "x".into(),
            };
            let mut results = Vec::new();
            for _ in 0..2 {
                results.push(handle_connection(
                    listener.accept().unwrap().0,
                    &info,
                    "secret",
                    |_| panic!("not a relaunch"),
                    &handler,
                ));
            }
            results
        });
        let request = |token: &str| {
            serde_json::to_vec(&ipc::RequestEnvelope {
                version: ipc::PROTOCOL_VERSION,
                token: token.into(),
                request: Request::ListDevices,
            })
            .unwrap()
        };
        let reply = String::from_utf8(send_command(port, &request("secret")).unwrap()).unwrap();
        let events: Vec<Event> = reply
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(events.len(), 2);
        assert!(matches!(events[1], Event::Devices { .. }));
        let reply = send_command(port, &request("wrong")).unwrap();
        assert!(matches!(
            serde_json::from_slice(reply.trim_ascii()).unwrap(),
            Event::Error { .. }
        ));
        let results = server.join().unwrap();
        assert!(results[0].is_ok() && results[1].is_err());
    }
    #[test]
    fn older_show_only_builds_are_recognized_without_sending_restart() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            stream.read_to_end(&mut request).unwrap();
            assert_eq!(request, b"info");
        });
        let response = send_command(port, b"info").unwrap();
        server.join().unwrap();
        assert!(serde_json::from_slice::<InstanceInfo>(&response).is_err());
        assert!(version_choice_required(None, "new-version"));
    }
    #[test]
    fn instance_handoff_waits_for_the_existing_owner() {
        let key = format!(
            "Jimpactor-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let first = SingleInstance::new(&key).unwrap();
        assert!(first.is_single());
        assert!(wait_for_instance(&key, Duration::from_millis(20)).is_err());
        drop(first);
        assert!(
            wait_for_instance(&key, Duration::from_secs(1))
                .unwrap()
                .is_single()
        );
    }
}
