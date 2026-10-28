use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use idevice::provider::{IdeviceProvider, TcpProvider};
use tokio::runtime::Handle;
use tokio::sync::watch;

use crate::Error;

#[derive(Clone)]
struct Endpoint {
    provider: Arc<TcpProvider>,
    runtime: Option<Handle>,
    online: bool,
    generation: u64,
}

/// Shared by cached devices and in-flight work, so a reconnect also updates
/// copies made before signing began. Credentials never appear in Debug output.
#[derive(Clone)]
pub struct NetworkConnection(Arc<watch::Sender<Endpoint>>);

impl fmt::Debug for NetworkConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let endpoint = self.0.borrow();
        f.debug_struct("NetworkConnection")
            .field("address", &endpoint.provider.addr)
            .field("online", &endpoint.online)
            .finish_non_exhaustive()
    }
}

impl NetworkConnection {
    pub(crate) fn new(provider: TcpProvider) -> Self {
        let (state, _) = watch::channel(Endpoint {
            provider: Arc::new(provider),
            runtime: Handle::try_current().ok(),
            online: true,
            generation: 0,
        });
        Self(Arc::new(state))
    }

    pub fn is_online(&self) -> bool {
        self.0.borrow().online
    }

    pub(crate) fn provider(&self, label: &str) -> TcpProvider {
        let state = self.0.borrow();
        clone_provider(&state.provider, label)
    }

    async fn ready(&self) -> Result<Endpoint, Error> {
        let mut state = self.0.subscribe();
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let endpoint = state.borrow_and_update().clone();
                if endpoint.online {
                    return Ok(endpoint);
                }
                state.changed().await.map_err(|_| Error::Other("Wi-Fi device worker stopped".into()))?;
            }
        }).await.map_err(|_| Error::Other(
            "Wi-Fi device is unavailable. Wake and unlock the iPhone, keep it on the same Wi-Fi, then retry.".into(),
        ))?
    }

    pub(crate) async fn execute<T, F, Fut>(&self, label: &str, operation: F) -> Result<T, Error>
    where
        T: Send + 'static,
        F: FnOnce(Box<dyn IdeviceProvider>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, Error>> + Send + 'static,
    {
        let endpoint = self.ready().await?;
        let provider = Box::new(clone_provider(&endpoint.provider, label));
        let future = operation(provider);
        match endpoint.runtime {
            Some(runtime) => runtime.spawn(future).await.map_err(|error| Error::Other(format!(
                "Wi-Fi device worker stopped: {error}. Wait for the iPhone to reconnect and retry."
            )))?,
            None => future.await,
        }
    }
}

fn clone_provider(provider: &TcpProvider, label: &str) -> TcpProvider {
    TcpProvider {
        addr: provider.addr,
        scope_id: provider.scope_id,
        pairing_file: provider.pairing_file.clone(),
        label: label.to_owned(),
    }
}

/// The latest authenticated session owns availability. An older worker finishing
/// after a replacement connected cannot mark the new session offline.
pub(crate) struct NetworkSession {
    connection: NetworkConnection,
    generation: u64,
}

impl NetworkSession {
    pub(crate) fn is_current(&self) -> bool {
        self.connection.0.borrow().generation == self.generation
    }
}

impl Drop for NetworkSession {
    fn drop(&mut self) {
        self.connection.0.send_modify(|state| {
            if state.generation == self.generation {
                state.online = false;
            }
        });
    }
}

#[derive(Clone, Default)]
pub(crate) struct NetworkConnections(Arc<Mutex<HashMap<String, Weak<watch::Sender<Endpoint>>>>>);

impl NetworkConnections {
    pub(crate) fn connected(
        &self,
        udid: &str,
        provider: TcpProvider,
    ) -> (NetworkConnection, NetworkSession) {
        let mut devices = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        devices.retain(|_, state| state.strong_count() > 0);
        let connection = if let Some(state) = devices.get(udid).and_then(Weak::upgrade) {
            let connection = NetworkConnection(state);
            connection.0.send_modify(|state| {
                state.provider = Arc::new(provider);
                state.runtime = Handle::try_current().ok();
                state.online = true;
                state.generation += 1;
            });
            connection
        } else {
            let connection = NetworkConnection::new(provider);
            devices.insert(udid.to_owned(), Arc::downgrade(&connection.0));
            connection
        };
        let generation = connection.0.borrow().generation;
        let session = NetworkSession {
            connection: connection.clone(),
            generation,
        };
        (connection, session)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::device_worker::DeviceWorker;
    use idevice::pairing_file::PairingFile;
    use plist::{Dictionary, Value};
    use tokio::sync::oneshot;

    pub(crate) fn fixture_pairing() -> PairingFile {
        let mut raw = Dictionary::new();
        for field in ["DeviceCertificate", "HostCertificate", "RootCertificate"] {
            raw.insert(
                field.into(),
                Value::Data(
                    b"-----BEGIN CERTIFICATE-----\nAQID\n-----END CERTIFICATE-----\n".to_vec(),
                ),
            );
        }
        for field in ["HostPrivateKey", "RootPrivateKey"] {
            raw.insert(field.into(), Value::Data(b"fixture-private-key".to_vec()));
        }
        raw.insert("SystemBUID".into(), "fixture-system".into());
        raw.insert("HostID".into(), "fixture-host".into());
        raw.insert("WiFiMACAddress".into(), "00:11:22:33:44:55".into());
        PairingFile::from_value(&raw.into()).unwrap()
    }

    fn provider(ip: &str) -> TcpProvider {
        TcpProvider {
            addr: ip.parse().unwrap(),
            scope_id: None,
            pairing_file: fixture_pairing(),
            label: "test".into(),
        }
    }

    #[tokio::test]
    async fn operations_run_on_the_owning_device_thread_and_shutdown_marks_it_offline() {
        let (tx, rx) = oneshot::channel();
        let worker = DeviceWorker::spawn("test-device-operations".into(), move || async move {
            let registry = NetworkConnections::default();
            let (connection, _session) = registry.connected("phone", provider("127.0.0.1"));
            tx.send((connection, std::thread::current().id())).unwrap();
            std::future::pending::<()>().await;
        })
        .unwrap();
        let (connection, expected_thread) = rx.await.unwrap();
        let operation_thread = connection
            .execute("test", |_| async { Ok(std::thread::current().id()) })
            .await
            .unwrap();
        assert_eq!(operation_thread, expected_thread);
        assert_ne!(operation_thread, std::thread::current().id());
        worker.shutdown().await;
        assert!(!connection.0.borrow().online);
    }

    #[tokio::test]
    async fn old_device_copies_follow_new_endpoint_and_ignore_stale_session_shutdown() {
        let registry = NetworkConnections::default();
        let (first, first_session) = registry.connected("phone", provider("127.0.0.1"));
        let cached = first.clone();
        let (_, second_session) = registry.connected("phone", provider("127.0.0.2"));
        assert!(!first_session.is_current());
        assert!(second_session.is_current());
        drop(first_session);
        assert_eq!(cached.provider("new label").addr.to_string(), "127.0.0.2");
        assert_eq!(cached.provider("new label").label, "new label");
        assert!(cached.0.borrow().online);
        drop(second_session);
        assert!(!cached.0.borrow().online);
    }

    #[tokio::test]
    async fn offline_operation_waits_for_reconnection_and_uses_refreshed_pairing() {
        let registry = NetworkConnections::default();
        let (connection, session) = registry.connected("phone", provider("127.0.0.1"));
        drop(session);
        let operation = connection.execute("test", |provider| async move {
            Ok(provider.get_pairing_file().await?.host_id)
        });
        tokio::pin!(operation);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut operation)
                .await
                .is_err()
        );
        let mut reconnected = provider("127.0.0.2");
        reconnected.pairing_file.host_id = "reconnected host".into();
        let (_, _session) = registry.connected("phone", reconnected);
        assert_eq!(operation.await.unwrap(), "reconnected host");
    }
}
