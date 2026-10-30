//! Local Mac anisette, generated in-process by Apple's Mac CoreADI under emulation.

use crate::{AnisetteError, anisette_headers_provider::AnisetteHeadersProvider, mac_libraries};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::mpsc,
    time::Duration,
};

pub const MAC_IDENTITY_DIR: &str = "mac-anisette";
pub const CLIENT_INFO: &str =
    "<MacBookPro13,2> <macOS;13.1;22C65> <com.apple.AuthKit/1 (com.apple.akd/1.0)>";
pub const USER_AGENT: &str = "akd/1.0 CFNetwork/1404.0.5 Darwin/22.3.0";

pub fn identity_path(root: &Path, fake: bool) -> PathBuf {
    if fake {
        root.join(crate::FAKE_IDENTITY_DIR).join(MAC_IDENTITY_DIR)
    } else {
        root.join(MAC_IDENTITY_DIR)
    }
}

#[derive(Serialize, Deserialize)]
struct Identity {
    host_uuid: [u8; 16],
    device_id: String,
    local_user_id: String,
    serial: String,
}

impl Identity {
    fn load(path: &Path) -> Result<Self, AnisetteError> {
        let file = path.join("identity.json");
        match std::fs::read(&file) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir_all(path)?;
                let identity = Self {
                    host_uuid: rand::random(),
                    device_id: format!("00006000-{}", hex::encode_upper(rand::random::<[u8; 8]>())),
                    local_user_id: hex::encode_upper(rand::random::<[u8; 16]>()),
                    serial: crate::adi_libraries::fake_serial_number(path)?,
                };
                save_json(&file, &identity)?;
                Ok(identity)
            }
            Err(error) => Err(error.into()),
        }
    }

    fn headers(&self) -> HashMap<String, String> {
        HashMap::from([
            ("X-Mme-Device-Id".into(), self.device_id.clone()),
            (
                "X-Apple-I-MD-LU".into(),
                STANDARD.encode(self.local_user_id.as_bytes()),
            ),
            ("X-Apple-I-SRL-NO".into(), self.serial.clone()),
            ("X-Mme-Client-Info".into(), CLIENT_INFO.into()),
            ("User-Agent".into(), USER_AGENT.into()),
            (
                "X-Apple-I-Client-Time".into(),
                chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            ),
            ("X-Apple-I-TimeZone".into(), "UTC".into()),
            ("X-Apple-Locale".into(), "en_US".into()),
        ])
    }
}

fn save_json(path: &Path, value: &impl Serialize) -> Result<(), AnisetteError> {
    let temporary = path.with_extension("partial");
    std::fs::write(&temporary, serde_json::to_vec(value)?)?;
    std::fs::rename(temporary, path)?;
    Ok(())
}

type EmulatorCall = Box<dyn FnOnce(&mut adi_core::CoreAdi) + Send>;

/// Own the non-Send emulator on one dedicated thread inside this process.
struct MacRuntime {
    requests: mpsc::Sender<EmulatorCall>,
}

impl MacRuntime {
    fn start(library: &Path) -> Result<Self, AnisetteError> {
        let image = std::fs::read(library)?;
        let (requests, operations) = mpsc::channel::<EmulatorCall>();
        let (initialized, ready) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("Mac CoreADI".into())
            .spawn(move || match adi_core::CoreAdi::new(&image) {
                Ok(mut adi) => {
                    if initialized.send(Ok(())).is_err() {
                        return;
                    }
                    for operation in operations {
                        operation(&mut adi);
                    }
                }
                Err(error) => {
                    let _ = initialized.send(Err(error));
                }
            })?;
        ready
            .recv_timeout(Duration::from_secs(45))
            .map_err(|_| anyhow::anyhow!("Mac CoreADI initialization exited or timed out"))?
            .map_err(adi_error)?;
        Ok(Self { requests })
    }

    fn call<R: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut adi_core::CoreAdi) -> Result<R, adi_core::Error> + Send + 'static,
    ) -> Result<R, AnisetteError> {
        let (send, reply) = mpsc::sync_channel(1);
        self.requests
            .send(Box::new(move |adi| {
                let _ = send.send(operation(adi));
            }))
            .map_err(|_| anyhow::anyhow!("Mac CoreADI worker stopped"))?;
        reply
            .recv_timeout(Duration::from_secs(45))
            .map_err(|_| anyhow::anyhow!("Mac CoreADI worker exited or timed out"))?
            .map_err(adi_error)
    }
}

fn adi_error(error: adi_core::Error) -> AnisetteError {
    match error {
        adi_core::Error::NotProvisioned => AnisetteError::AnisetteNotProvisioned,
        other => anyhow::anyhow!("Mac CoreADI: {other}").into(),
    }
}

pub struct MacAnisetteProvider {
    root: PathBuf,
    identity_path: PathBuf,
}

impl MacAnisetteProvider {
    pub fn new(root: PathBuf, fake: bool) -> Self {
        Self {
            identity_path: identity_path(&root, fake),
            root,
        }
    }

    fn save_store(&self, runtime: &MacRuntime) -> Result<(), AnisetteError> {
        let files = runtime.call(|adi| Ok(adi.store_files()))?;
        save_json(&self.identity_path.join("store.json"), &files)
    }

    #[cfg_attr(not(feature = "async"), remove_async_await::remove_async_await)]
    async fn provision(
        &self,
        runtime: &MacRuntime,
        identity: &Identity,
    ) -> Result<(), AnisetteError> {
        #[cfg(feature = "async")]
        use reqwest::Client;
        #[cfg(not(feature = "async"))]
        use reqwest::blocking::Client;
        let mut headers = reqwest::header::HeaderMap::new();
        for (name, value) in identity.headers() {
            headers.insert(
                reqwest::header::HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
                    AnisetteError::InvalidArgument("Invalid local header name".into())
                })?,
                reqwest::header::HeaderValue::from_str(&value).map_err(|_| {
                    AnisetteError::InvalidArgument("Invalid local header value".into())
                })?,
            );
        }
        let client = Client::builder()
            .add_root_certificate(reqwest::Certificate::from_der(include_bytes!(
                "apple_root.der"
            ))?)
            .add_root_certificate(reqwest::Certificate::from_der(include_bytes!(
                "apple_root_g3.der"
            ))?)
            .default_headers(headers)
            .timeout(Duration::from_secs(60))
            .http1_title_case_headers()
            .build()?;
        let response = client
            .get("https://gsa.apple.com/grandslam/GsService2/lookup")
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        let lookup: plist::Dictionary = plist::from_bytes(&response)?;
        let urls = lookup
            .get("urls")
            .and_then(plist::Value::as_dictionary)
            .ok_or_else(|| anyhow::anyhow!("Missing Apple provisioning URLs"))?;
        let url = |key| {
            urls.get(key)
                .and_then(plist::Value::as_string)
                .ok_or_else(|| anyhow::anyhow!("Missing Apple provisioning URL: {key}"))
        };
        let body = plist::Dictionary::from_iter([
            ("Header", plist::Value::Dictionary(plist::Dictionary::new())),
            (
                "Request",
                plist::Value::Dictionary(plist::Dictionary::new()),
            ),
        ]);
        let mut buffer = Vec::new();
        plist::to_writer_xml(&mut buffer, &body)?;
        let response = client
            .post(url("midStartProvisioning")?)
            .header("Content-Type", "text/x-xml-plist")
            .body(buffer)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        let response = provision_response(&response)?;
        let spim = plist_data(&response, "spim")?;
        let (cpim, session) = runtime.call(move |adi| adi.provisioning_begin(-2, &spim))?;
        let mut body = body;
        body.insert(
            "Request".into(),
            plist::Value::Dictionary(plist::Dictionary::from_iter([(
                "cpim",
                plist::Value::String(STANDARD.encode(cpim)),
            )])),
        );
        let mut buffer = Vec::new();
        plist::to_writer_xml(&mut buffer, &body)?;
        let response = client
            .post(url("midFinishProvisioning")?)
            .header("Content-Type", "text/x-xml-plist")
            .body(buffer)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        let response = provision_response(&response)?;
        let routing = response
            .get("X-Apple-I-MD-RINFO")
            .and_then(|value| {
                value
                    .as_unsigned_integer()
                    .or_else(|| value.as_string().and_then(|s| s.parse().ok()))
            })
            .ok_or_else(|| anyhow::anyhow!("Missing Apple-assigned routing information"))?;
        let ptm = plist_data(&response, "ptm")?;
        let tk = plist_data(&response, "tk")?;
        runtime.call(move |adi| {
            adi.provisioning_finish(-2, session, &ptm, &tk, routing)?;
            // The legacy whole-machine blob masks the new per-user store.
            adi.vm()
                .files_mut()
                .remove(&format!("{}/adi.pb", adi_core::STORE_DIR));
            Ok(())
        })?;
        self.save_store(runtime)?;
        Ok(())
    }
}

fn provision_response(bytes: &[u8]) -> Result<plist::Dictionary, AnisetteError> {
    let outer: plist::Dictionary = plist::from_bytes(bytes)?;
    let response = outer
        .get("Response")
        .and_then(plist::Value::as_dictionary)
        .ok_or_else(|| anyhow::anyhow!("Missing Apple provisioning response"))?;
    let status = response
        .get("Status")
        .and_then(plist::Value::as_dictionary)
        .ok_or_else(|| anyhow::anyhow!("Missing Apple provisioning status"))?;
    let code = status
        .get("ec")
        .and_then(plist::Value::as_signed_integer)
        .ok_or_else(|| anyhow::anyhow!("Missing Apple provisioning status code"))?;
    if code != 0 {
        return Err(anyhow::anyhow!("Apple refused Mac provisioning ({code})").into());
    }
    Ok(response.clone())
}

fn plist_data(response: &plist::Dictionary, key: &str) -> Result<Vec<u8>, AnisetteError> {
    let value = response
        .get(key)
        .ok_or_else(|| anyhow::anyhow!("Missing Apple provisioning field: {key}"))?;
    if let Some(bytes) = value.as_data() {
        return Ok(bytes.to_vec());
    }
    let text = value
        .as_string()
        .ok_or_else(|| anyhow::anyhow!("Invalid Apple provisioning field: {key}"))?;
    STANDARD
        .decode(text)
        .map_err(|error| anyhow::anyhow!("Invalid Apple provisioning data: {error}").into())
}

#[cfg_attr(feature = "async", async_trait::async_trait)]
impl AnisetteHeadersProvider for MacAnisetteProvider {
    #[cfg_attr(not(feature = "async"), remove_async_await::remove_async_await)]
    async fn get_anisette_headers(
        &mut self,
        skip_provisioning: bool,
    ) -> Result<HashMap<String, String>, AnisetteError> {
        let identity = Identity::load(&self.identity_path)?;
        let runtime = MacRuntime::start(&mac_libraries::library_path(&self.root))?;
        let store_path = self.identity_path.join("store.json");
        let files: Vec<(String, Vec<u8>)> = match std::fs::read(store_path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error.into()),
        };
        let uuid = identity.host_uuid;
        for (name, _) in &files {
            if name.contains(['/', '\\']) || name == "." || name == ".." {
                return Err(AnisetteError::InvalidArgument(
                    "Invalid Mac provisioning store filename".into(),
                ));
            }
        }
        runtime.call(move |adi| {
            adi.set_host_uuid(uuid);
            for (name, data) in files {
                adi.store_put(&name, &data);
            }
            Ok(())
        })?;
        let result = runtime.call(|adi| adi.headers(-2));
        let result = match result {
            Err(AnisetteError::AnisetteNotProvisioned) if !skip_provisioning => {
                self.provision(&runtime, &identity).await?;
                runtime.call(|adi| adi.headers(-2))?
            }
            other => other?,
        };
        self.save_store(&runtime)?;
        let adi_core::Headers { mid, otp, routing } = result;
        if mid.is_empty() || otp.is_empty() {
            return Err(anyhow::anyhow!("Mac CoreADI returned empty anisette data").into());
        }
        let mut headers = identity.headers();
        headers.insert("X-Apple-I-MD-M".into(), STANDARD.encode(mid));
        headers.insert("X-Apple-I-MD".into(), STANDARD.encode(otp));
        headers.insert("X-Apple-I-MD-RINFO".into(), routing.to_string());
        Ok(headers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_identity_persists_without_reusing_android_or_remote_stores() {
        let root = std::env::temp_dir().join(format!("jimpactor-mac-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        for name in ["identifier", "adi.pb", crate::ANISETTE_STATE_FILE] {
            std::fs::write(root.join(name), "legacy").unwrap();
        }
        let local = identity_path(&root, false);
        let fake = identity_path(&root, true);
        let identity = Identity::load(&local).unwrap();
        let reloaded = Identity::load(&local).unwrap();
        let other = Identity::load(&fake).unwrap();
        assert_eq!(identity.host_uuid, reloaded.host_uuid);
        assert_eq!(identity.device_id, reloaded.device_id);
        assert_ne!(identity.host_uuid, other.host_uuid);
        let headers = identity.headers();
        assert_eq!(headers["X-Mme-Client-Info"], CLIENT_INFO);
        assert_eq!(headers["X-Apple-I-SRL-NO"].len(), 12);
        assert!(headers["X-Mme-Device-Id"].starts_with("00006000-"));
        assert_eq!(headers["X-Mme-Device-Id"].len(), 25);
        assert_eq!(
            STANDARD.decode(&headers["X-Apple-I-MD-LU"]).unwrap(),
            identity.local_user_id.as_bytes()
        );
        for name in ["identifier", "adi.pb", crate::ANISETTE_STATE_FILE] {
            assert_eq!(std::fs::read_to_string(root.join(name)).unwrap(), "legacy");
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn provisioning_errors_and_binary_data_are_parsed_without_panics() {
        let mut body = plist::Dictionary::new();
        body.insert(
            "Response".into(),
            plist::Value::Dictionary(plist::Dictionary::from_iter([(
                "Status",
                plist::Value::Dictionary(plist::Dictionary::from_iter([(
                    "ec",
                    plist::Value::Integer((-45003i64).into()),
                )])),
            )])),
        );
        let mut bytes = Vec::new();
        plist::to_writer_xml(&mut bytes, &body).unwrap();
        assert!(
            provision_response(&bytes)
                .unwrap_err()
                .to_string()
                .contains("-45003")
        );
        assert!(provision_response(b"<html>error</html>").is_err());
        let response = plist::Dictionary::from_iter([
            ("spim", plist::Value::String(STANDARD.encode([1, 2, 3]))),
            ("tk", plist::Value::Data(vec![4, 5])),
        ]);
        assert_eq!(plist_data(&response, "spim").unwrap(), [1, 2, 3]);
        assert_eq!(plist_data(&response, "tk").unwrap(), [4, 5]);
        assert!(plist_data(&response, "missing").is_err());
    }
}
