//! Reconnecting usbmuxd discovery shared by device consumers.

use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use futures::StreamExt;
use idevice::usbmuxd::{UsbmuxdAddr, UsbmuxdListenEvent};
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::{AbortHandle, JoinSet};

use crate::{Device, Error};

#[derive(Debug)]
pub enum DeviceEvent {
    Connected(Device),
    Disconnected(u32),
}

/// Watches attached devices until the receiver is dropped. `Listen` reports devices
/// already attached as well as subsequent changes, avoiding a list/subscribe race.
/// The caller must poll this on a local runtime: idevice's listen stream isn't Send.
pub async fn watch_devices(address: UsbmuxdAddr, output: UnboundedSender<DeviceEvent>) {
    loop {
        let mut connected = BTreeSet::new();
        tokio::select! {
            _ = output.closed() => return,
            result = watch_connection(&address, &output, &mut connected) => {
                if let Err(error) = result {
                    log::warn!("Device discovery interrupted: {error}; reconnecting in 2 seconds");
                }
            }
        }

        // A lost mux connection invalidates its device IDs. Repopulate from the
        // new listener, including removals that happened while it was unavailable.
        for id in connected {
            let _ = output.send(DeviceEvent::Disconnected(id));
        }
        tokio::select! {
            _ = output.closed() => return,
            _ = tokio::time::sleep(Duration::from_secs(2)) => {}
        }
    }
}

async fn watch_connection(
    address: &UsbmuxdAddr,
    output: &UnboundedSender<DeviceEvent>,
    connected: &mut BTreeSet<u32>,
) -> Result<(), Error> {
    let mut muxer = tokio::time::timeout(Duration::from_secs(5), address.connect(0))
        .await
        .map_err(|_| Error::Other("Timed out connecting to usbmuxd".into()))??;
    let mut stream = tokio::time::timeout(Duration::from_secs(5), muxer.listen())
        .await
        .map_err(|_| Error::Other("Timed out subscribing to usbmuxd".into()))??;
    log::info!("Listening for devices through {address:?}");

    let mut pending = HashMap::<u32, AbortHandle>::new();
    let mut initialized = JoinSet::new();
    loop {
        tokio::select! {
            event = stream.next() => match event {
                Some(Ok(UsbmuxdListenEvent::Connected(device))) => {
                    let id = device.device_id;
                    if connected.contains(&id) || pending.contains_key(&id) {
                        continue;
                    }
                    let address = address.clone();
                    pending.insert(id, initialized.spawn(Device::with_address(device, address)));
                }
                Some(Ok(UsbmuxdListenEvent::Disconnected(id))) => {
                    if let Some(task) = pending.remove(&id) {
                        task.abort();
                    }
                    if connected.remove(&id) {
                        log::info!("Device disconnected: {id}");
                        let _ = output.send(DeviceEvent::Disconnected(id));
                    }
                }
                Some(Err(error)) => return Err(error.into()),
                None => return Err(Error::Other("usbmuxd device stream ended".into())),
            },
            result = initialized.join_next_with_id(), if !initialized.is_empty() => {
                match result {
                    Some(Ok((task_id, device))) => {
                        // A detach can arrive after initialization completed but
                        // before its result was consumed. Never restore that device.
                        if pending.get(&device.device_id).map(AbortHandle::id) != Some(task_id) {
                            continue;
                        }
                        pending.remove(&device.device_id);
                        connected.insert(device.device_id);
                        log::info!("Device connected: {device}");
                        let _ = output.send(DeviceEvent::Connected(device));
                    }
                    Some(Err(error)) if !error.is_cancelled() => {
                        return Err(Error::Other(format!("Device initialization failed: {error}")));
                    }
                    _ => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plist::{Dictionary, Value};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::mpsc::{self, UnboundedReceiver};

    fn dict<const N: usize>(entries: [(&str, Value); N]) -> Dictionary {
        entries.into_iter().collect()
    }

    async fn read_mux(stream: &mut TcpStream) -> Dictionary {
        let length = stream.read_u32_le().await.unwrap() as usize;
        let mut rest = vec![0; length - 4];
        stream.read_exact(&mut rest).await.unwrap();
        plist::from_bytes(&rest[12..]).unwrap()
    }

    async fn write_mux(stream: &mut TcpStream, value: Dictionary) {
        let mut bytes = Vec::new();
        Value::Dictionary(value).to_writer_xml(&mut bytes).unwrap();
        stream.write_u32_le(bytes.len() as u32 + 16).await.unwrap();
        stream.write_u32_le(1).await.unwrap();
        stream.write_u32_le(8).await.unwrap();
        stream.write_u32_le(0).await.unwrap();
        stream.write_all(&bytes).await.unwrap();
    }

    fn attached(id: u32) -> Dictionary {
        let properties = dict([
            ("ConnectionType", "USB".into()),
            ("SerialNumber", format!("test-device-{id}").into()),
        ]);
        dict([
            ("MessageType", "Attached".into()),
            ("DeviceID", id.into()),
            ("Properties", properties.into()),
        ])
    }

    // Implements the mux and lockdown wire protocols rather than mocking the
    // discovery functions. Device 2 deliberately never answers its name lookup.
    async fn fake_mux() -> (UsbmuxdAddr, UnboundedReceiver<TcpStream>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = UsbmuxdAddr::TcpSocket(listener.local_addr().unwrap());
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let tx = tx.clone();
                tokio::spawn(async move {
                    let request = read_mux(&mut stream).await;
                    let success = dict([("MessageType", "Result".into()), ("Number", 0.into())]);
                    match request.get("MessageType").and_then(Value::as_string) {
                        Some("Listen") => {
                            write_mux(&mut stream, success).await;
                            let _ = tx.send(stream);
                        }
                        Some("Connect") => {
                            write_mux(&mut stream, success).await;
                            if request.get("DeviceID").and_then(Value::as_unsigned_integer)
                                == Some(2)
                            {
                                std::future::pending::<()>().await;
                            }
                            let length = stream.read_u32().await.unwrap();
                            let mut bytes = vec![0; length as usize];
                            stream.read_exact(&mut bytes).await.unwrap();
                            let request: Dictionary = plist::from_bytes(&bytes).unwrap();
                            assert_eq!(
                                request.get("Key").and_then(Value::as_string),
                                Some("DeviceName")
                            );
                            let response = dict([("Value", "Test iPhone".into())]);
                            bytes.clear();
                            Value::Dictionary(response)
                                .to_writer_xml(&mut bytes)
                                .unwrap();
                            stream.write_u32(bytes.len() as u32).await.unwrap();
                            stream.write_all(&bytes).await.unwrap();
                        }
                        request => panic!("Unexpected discovery request: {request:?}"),
                    }
                });
            }
        });
        (address, rx)
    }

    async fn receive(rx: &mut UnboundedReceiver<DeviceEvent>) -> DeviceEvent {
        tokio::time::timeout(Duration::from_secs(6), rx.recv())
            .await
            .expect("discovery stopped delivering events")
            .expect("discovery closed")
    }

    #[tokio::test]
    async fn discovers_existing_devices_and_reconnects_after_service_disconnect() {
        let (address, mut listeners) = fake_mux().await;
        let (tx, mut rx) = mpsc::unbounded_channel();
        tokio::join!(watch_devices(address, tx), async move {
            let mut stream = listeners.recv().await.unwrap();
            write_mux(&mut stream, attached(1)).await;
            let DeviceEvent::Connected(device) = receive(&mut rx).await else {
                panic!("expected device");
            };
            assert_eq!(device.name, "Test iPhone");
            assert_eq!(device.device_id, 1);
            drop(stream);
            assert!(matches!(
                receive(&mut rx).await,
                DeviceEvent::Disconnected(1)
            ));
            let mut stream = tokio::time::timeout(Duration::from_secs(6), listeners.recv())
                .await
                .unwrap()
                .unwrap();
            write_mux(&mut stream, attached(3)).await;
            assert!(matches!(
                receive(&mut rx).await,
                DeviceEvent::Connected(Device { device_id: 3, .. })
            ));
            drop(rx);
        });
    }

    #[tokio::test]
    async fn stalled_name_lookup_does_not_block_other_devices_and_falls_back_to_udid() {
        let (address, mut listeners) = fake_mux().await;
        let (tx, mut rx) = mpsc::unbounded_channel();
        tokio::join!(watch_devices(address, tx), async move {
            let mut stream = listeners.recv().await.unwrap();
            write_mux(&mut stream, attached(2)).await;
            write_mux(&mut stream, attached(1)).await;
            assert!(matches!(
                receive(&mut rx).await,
                DeviceEvent::Connected(Device { device_id: 1, .. })
            ));
            let DeviceEvent::Connected(device) = receive(&mut rx).await else {
                panic!("expected fallback device");
            };
            assert_eq!(device.device_id, 2);
            assert_eq!(device.name, "test-device-2");
            drop(rx);
        });
    }

    #[tokio::test]
    async fn detached_device_is_not_restored_by_pending_name_lookup() {
        let (address, mut listeners) = fake_mux().await;
        let (tx, mut rx) = mpsc::unbounded_channel();
        tokio::join!(watch_devices(address, tx), async move {
            let mut stream = listeners.recv().await.unwrap();
            write_mux(&mut stream, attached(2)).await;
            write_mux(
                &mut stream,
                dict([("MessageType", "Detached".into()), ("DeviceID", 2.into())]),
            )
            .await;
            write_mux(&mut stream, attached(1)).await;
            assert!(matches!(
                receive(&mut rx).await,
                DeviceEvent::Connected(Device { device_id: 1, .. })
            ));
            assert!(
                tokio::time::timeout(Duration::from_millis(3300), rx.recv())
                    .await
                    .is_err()
            );
            drop(rx);
        });
    }

    #[tokio::test]
    #[ignore = "requires a connected iOS device and the local Apple device service"]
    async fn live_device_discovery() {
        let address = UsbmuxdAddr::from_env_var().unwrap_or_default();
        let mut muxer = address.connect(0).await.unwrap();
        let devices = muxer.get_devices().await.unwrap();
        assert!(
            !devices.is_empty(),
            "Connect an iOS device before running this test"
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        tokio::join!(watch_devices(address, tx), async move {
            let DeviceEvent::Connected(device) = receive(&mut rx).await else {
                panic!("expected live device");
            };
            assert_ne!(device.name, device.udid, "Device name lookup failed");
            println!("Discovered {device}");
            drop(rx);
        });
    }
}
