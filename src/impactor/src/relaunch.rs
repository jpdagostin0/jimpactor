#![cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]

use serde::{Deserialize, Serialize};
use single_instance::SingleInstance;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const RELAUNCH_PORT_FILE: &str = "relaunch.port";
#[cfg(target_os = "macos")]
const SINGLE_INSTANCE_FILE: &str = "single-instance.lock";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RelaunchAction {
    Show,
    Restart,
}

#[derive(Debug, Serialize, Deserialize)]
struct InstanceInfo {
    version: String,
    executable: PathBuf,
}

fn relaunch_port_path() -> PathBuf {
    crate::defaults::get_data_path().join(RELAUNCH_PORT_FILE)
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
    action: impl Fn(RelaunchAction),
) -> Result<(), String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    let mut command = Vec::new();
    (&mut stream)
        .take(65)
        .read_to_end(&mut command)
        .map_err(|e| e.to_string())?;
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
        _ => Err("Unsupported relaunch command".into()),
    }
}

pub(crate) fn start_listener<F>(on_action: F) -> Result<(), String>
where
    F: Fn(RelaunchAction) + Send + Sync + 'static,
{
    let listener =
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| format!("bind listener: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let info = InstanceInfo {
        version: env!("CARGO_PKG_VERSION").into(),
        executable: std::env::current_exe().map_err(|e| e.to_string())?,
    };
    std::fs::write(relaunch_port_path(), port.to_string())
        .map_err(|e| format!("write port: {e}"))?;
    std::thread::spawn(move || {
        for incoming in listener.incoming() {
            match incoming {
                Ok(stream) => {
                    if let Err(error) = handle_connection(stream, &info, &on_action) {
                        log::warn!("Failed to handle relaunch request: {error}");
                    }
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
                handle_connection(listener.accept().unwrap().0, &info, |action| {
                    tx.send(action).unwrap()
                })
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
