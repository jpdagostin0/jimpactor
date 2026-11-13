//! Local IPC with a running Jimpactor (the tray app), so other programs can send it IPAs.
//!
//! The app listens on 127.0.0.1 at the port written to `<data dir>/relaunch.port`. A client
//! connects, writes one JSON [`RequestEnvelope`], shuts down its write half, then reads
//! newline-delimited JSON [`Event`]s until a terminal event (`info`, `devices`, `done` or
//! `error`) arrives and the app closes the connection. The JSON Schema in `ipc.schema.json`
//! ([`SCHEMA`]) describes the same messages for clients written in other languages.
//!
//! Every JSON request carries the secret from `<data dir>/ipc.token`, which only the user
//! running Jimpactor can read, since any local user can connect to a loopback port.
//!
//! The plain-text `info`, `show` and `restart` commands used between Jimpactor builds are
//! older than this protocol and remain unauthenticated.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, Shutdown, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Bumped when a request or event changes incompatibly.
pub const PROTOCOL_VERSION: u32 = 1;
/// Holds the loopback port the running app listens on.
pub const PORT_FILE: &str = "relaunch.port";
/// Holds the secret that JSON requests must carry.
pub const TOKEN_FILE: &str = "ipc.token";
/// Largest request the app reads; anything longer is rejected.
pub const MAX_REQUEST_BYTES: u64 = 64 * 1024;
/// JSON Schema for [`RequestEnvelope`] and [`Event`].
pub const SCHEMA: &str = include_str!("../ipc.schema.json");

/// What a client sends: one JSON object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestEnvelope {
    /// The [`PROTOCOL_VERSION`] the client speaks.
    pub version: u32,
    /// The contents of [`TOKEN_FILE`].
    pub token: String,
    #[serde(flatten)]
    pub request: Request,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Request {
    /// Answered with [`Event::Info`].
    Info,
    /// Answered with [`Event::Devices`].
    ListDevices,
    /// Signs and installs an IPA, answered with [`Event::Progress`] updates and then
    /// [`Event::Done`] or [`Event::Error`]. The install keeps going if the client disconnects.
    Install {
        /// Absolute path to the IPA, readable by the user running Jimpactor.
        path: PathBuf,
        /// Target device. When omitted, the locked device (if device locking is on) or the
        /// only connected device is used.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        udid: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Info {
        app_version: String,
        protocol_version: u32,
    },
    Devices {
        devices: Vec<DeviceSummary>,
    },
    /// `percent` runs from 0 to 100.
    Progress {
        message: String,
        percent: i32,
    },
    Done {
        message: String,
    },
    Error {
        message: String,
    },
}

impl Event {
    /// Whether this event ends the response.
    pub fn is_terminal(&self) -> bool {
        !matches!(self, Event::Progress { .. })
    }

    pub fn error(message: impl Into<String>) -> Self {
        Event::Error {
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceSummary {
    pub udid: String,
    pub name: String,
    /// "usb", "wifi", "tailscale" or "local" (this Mac).
    pub connection: String,
    /// Whether the device can be reached right now.
    pub available: bool,
}

impl From<&crate::Device> for DeviceSummary {
    fn from(device: &crate::Device) -> Self {
        use idevice::usbmuxd::Connection;
        let connection = match (&device.usbmuxd_device, &device.network) {
            (Some(usb), _) => match usb.connection_type {
                Connection::Usb => "usb",
                Connection::Network(_) => "wifi",
                Connection::Unknown(_) => "unknown",
            },
            (None, Some(network)) if network.is_tailnet() => "tailscale",
            (None, Some(_)) => "wifi",
            (None, None) => "local",
        };
        Self {
            udid: device.udid.clone(),
            name: device.name.clone(),
            connection: connection.into(),
            available: device.is_available(),
        }
    }
}

/// Writes one event as a JSON line.
pub fn write_event(mut writer: impl Write, event: &Event) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(event)?;
    line.push(b'\n');
    writer.write_all(&line)?;
    writer.flush()
}

/// Reads the port and token of the Jimpactor running for this user.
pub fn endpoint(data_path: &Path) -> Result<(u16, String), String> {
    let port = std::fs::read_to_string(data_path.join(PORT_FILE))
        .map_err(|_| "Jimpactor is not running".to_string())?
        .trim()
        .parse()
        .map_err(|e| format!("Invalid {PORT_FILE}: {e}"))?;
    let token = std::fs::read_to_string(data_path.join(TOKEN_FILE))
        .map_err(|_| "The running Jimpactor is too old to accept IPC requests".to_string())?
        .trim()
        .to_owned();
    Ok((port, token))
}

/// Sends `request` to the running app, passing each progress event to `on_progress`, and
/// returns the terminal event. [`Event::Error`] is returned as `Err`.
pub fn send(
    data_path: &Path,
    request: Request,
    mut on_progress: impl FnMut(&str, i32),
) -> Result<Event, String> {
    let (port, token) = endpoint(data_path)?;
    let stream =
        TcpStream::connect_timeout(&(Ipv4Addr::LOCALHOST, port).into(), Duration::from_secs(2))
            .map_err(|e| format!("Could not reach Jimpactor (is it running?): {e}"))?;
    let envelope = RequestEnvelope {
        version: PROTOCOL_VERSION,
        token,
        request,
    };
    (&stream)
        .write_all(&serde_json::to_vec(&envelope).map_err(|e| e.to_string())?)
        .and_then(|()| stream.shutdown(Shutdown::Write))
        .map_err(|e| format!("Could not send the request: {e}"))?;

    for line in BufReader::new(stream).lines() {
        let line = line.map_err(|e| format!("Lost the connection to Jimpactor: {e}"))?;
        if line.trim().is_empty() {
            continue;
        }
        let event: Event = serde_json::from_str(&line)
            .map_err(|e| format!("Unexpected reply from Jimpactor: {e}: {line}"))?;
        match event {
            Event::Progress { message, percent } => on_progress(&message, percent),
            Event::Error { message } => return Err(message),
            event => return Ok(event),
        }
    }
    Err("Jimpactor closed the connection before finishing".into())
}

/// Reads a request of at most [`MAX_REQUEST_BYTES`] from `reader`.
pub fn read_request(reader: impl Read) -> std::io::Result<Vec<u8>> {
    let mut request = Vec::new();
    reader
        .take(MAX_REQUEST_BYTES + 1)
        .read_to_end(&mut request)?;
    if request.len() as u64 > MAX_REQUEST_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "IPC request is too large",
        ));
    }
    Ok(request)
}

/// Parses and authenticates a JSON request.
pub fn parse_request(bytes: &[u8], token: &str) -> Result<Request, String> {
    let envelope: RequestEnvelope =
        serde_json::from_slice(bytes).map_err(|e| format!("Invalid request: {e}"))?;
    if envelope.version != PROTOCOL_VERSION {
        return Err(format!(
            "Unsupported protocol version {} (this Jimpactor speaks {PROTOCOL_VERSION})",
            envelope.version
        ));
    }
    if !constant_time_eq(envelope.token.as_bytes(), token.as_bytes()) {
        return Err("Invalid token".into());
    }
    Ok(envelope.request)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Creates a fresh token and writes it where only this user can read it.
pub fn write_token(data_path: &Path) -> std::io::Result<String> {
    let token = uuid::Uuid::new_v4().simple().to_string();
    let path = data_path.join(TOKEN_FILE);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        options.mode(0o600);
        // `mode` only applies on creation, so tighten a file left by an earlier run too.
        if path.exists() {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
    }
    options.open(&path)?.write_all(token.as_bytes())?;
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn requests_and_events_use_the_documented_wire_format() {
        let install = RequestEnvelope {
            version: 1,
            token: "t".into(),
            request: Request::Install {
                path: "/tmp/App.ipa".into(),
                udid: None,
            },
        };
        assert_eq!(
            serde_json::to_value(&install).unwrap(),
            serde_json::json!({"version": 1, "token": "t", "command": "install", "path": "/tmp/App.ipa"})
        );
        let parsed: RequestEnvelope = serde_json::from_str(
            r#"{"version":1,"token":"t","command":"install","path":"/a.ipa","udid":"X"}"#,
        )
        .unwrap();
        assert_eq!(
            parsed.request,
            Request::Install {
                path: "/a.ipa".into(),
                udid: Some("X".into())
            }
        );
        assert_eq!(
            serde_json::to_value(Event::Progress {
                message: "Signing".into(),
                percent: 70
            })
            .unwrap(),
            serde_json::json!({"event": "progress", "message": "Signing", "percent": 70})
        );
    }

    /// Keeps `ipc.schema.json` in step with the Rust types.
    #[test]
    fn schema_lists_every_command_and_event() {
        let schema: serde_json::Value = serde_json::from_str(SCHEMA).unwrap();
        let consts = |key: &str, field: &str| -> Vec<String> {
            schema["$defs"][key]["oneOf"]
                .as_array()
                .unwrap()
                .iter()
                .map(|variant| {
                    let reference = variant["$ref"].as_str().unwrap();
                    let name = reference.rsplit('/').next().unwrap();
                    schema["$defs"][name]["properties"][field]["const"]
                        .as_str()
                        .unwrap()
                        .to_owned()
                })
                .collect()
        };
        let tag = |value: serde_json::Value, field: &str| value[field].as_str().unwrap().to_owned();

        let requests = [
            Request::Info,
            Request::ListDevices,
            Request::Install {
                path: "a".into(),
                udid: None,
            },
        ];
        let commands: Vec<_> = requests
            .into_iter()
            .map(|r| tag(serde_json::to_value(r).unwrap(), "command"))
            .collect();
        assert_eq!(consts("request", "command"), commands);

        let events = [
            Event::Info {
                app_version: "1".into(),
                protocol_version: 1,
            },
            Event::Devices { devices: vec![] },
            Event::Progress {
                message: "".into(),
                percent: 0,
            },
            Event::Done { message: "".into() },
            Event::error(""),
        ];
        let names: Vec<_> = events
            .into_iter()
            .map(|e| tag(serde_json::to_value(e).unwrap(), "event"))
            .collect();
        assert_eq!(consts("event", "event"), names);
        assert_eq!(
            schema["$defs"]["envelope"]["properties"]["version"]["const"],
            PROTOCOL_VERSION
        );
    }

    #[test]
    fn requests_need_the_token_and_protocol_version() {
        let request = |version: u32, token: &str| {
            serde_json::to_vec(&RequestEnvelope {
                version,
                token: token.into(),
                request: Request::ListDevices,
            })
            .unwrap()
        };
        assert_eq!(
            parse_request(&request(1, "secret"), "secret"),
            Ok(Request::ListDevices)
        );
        assert!(parse_request(&request(1, "wrong"), "secret").is_err());
        assert!(parse_request(&request(2, "secret"), "secret").is_err());
        assert!(parse_request(b"show", "secret").is_err());
        assert!(read_request(&vec![b' '; MAX_REQUEST_BYTES as usize + 1][..]).is_err());
    }

    #[test]
    fn client_streams_progress_until_a_terminal_event() {
        let dir = tempfile::tempdir().unwrap();
        let token = write_token(dir.path()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join(TOKEN_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::fs::write(dir.path().join(PORT_FILE), port.to_string()).unwrap();

        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let bytes = read_request(&mut stream).unwrap();
            let request = parse_request(&bytes, &token).unwrap();
            for percent in [10, 50] {
                write_event(
                    &mut stream,
                    &Event::Progress {
                        message: format!("{percent}"),
                        percent,
                    },
                )
                .unwrap();
            }
            write_event(
                &mut stream,
                &Event::Done {
                    message: "ok".into(),
                },
            )
            .unwrap();
            request
        });

        let mut progress = Vec::new();
        let result = send(
            dir.path(),
            Request::Install {
                path: "/x.ipa".into(),
                udid: None,
            },
            |_, percent| progress.push(percent),
        );
        assert_eq!(
            result,
            Ok(Event::Done {
                message: "ok".into()
            })
        );
        assert_eq!(progress, [10, 50]);
        assert!(matches!(server.join().unwrap(), Request::Install { .. }));
    }

    #[test]
    fn error_events_become_errors() {
        let dir = tempfile::tempdir().unwrap();
        write_token(dir.path()).unwrap();
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::fs::write(dir.path().join(PORT_FILE), port.to_string()).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read_request(&mut stream).unwrap();
            write_event(&mut stream, &Event::error("No device")).unwrap();
        });
        assert_eq!(
            send(dir.path(), Request::ListDevices, |_, _| {}),
            Err("No device".into())
        );
        server.join().unwrap();
        assert!(send(&dir.path().join("missing"), Request::Info, |_, _| {}).is_err());
    }
}
