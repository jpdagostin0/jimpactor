//! Classic "v1" anisette provider: a plain HTTPS GET that returns the anisette
//! headers as a flat JSON object. This is the format used by the original
//! anisette servers and by Sideloadly's remote endpoint
//! (`.../anisette/<id>?u=<device-hash>`).
//!
//! No token or account is sent — this is the free, rate-limited path. A random
//! per-install device hash is persisted so the server returns a stable identity
//! across refreshes.

use std::collections::HashMap;
use std::path::PathBuf;

use async_trait::async_trait;
use log::debug;
use reqwest::Client;

use crate::AnisetteError;
use crate::anisette_headers_provider::AnisetteHeadersProvider;

const DEVICE_HASH_FILE: &str = "anisette_v1_device";

/// Persists a validated replacement device identity after a successful refresh.
pub fn save_device_hash(path: &std::path::Path, hash: &str) -> Result<(), AnisetteError> {
    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(AnisetteError::InvalidArgument("invalid device hash".into()));
    }
    std::fs::create_dir_all(path)?;
    std::fs::write(path.join(DEVICE_HASH_FILE), hash)?;
    Ok(())
}

/// Default Sideloadly instance. The remote anisette endpoint is just a classic v1
/// server, so any self-hosted instance running the same software works too; this
/// is only the fallback used when a caps-token is given with no host of its own.
pub const DEFAULT_SIDELOADLY_HOST: &str = "sideloadly.io";

/// Builds the v1 anisette URL for a Sideloadly(-compatible) instance from a host
/// (with or without a scheme) and a caps-token, e.g.
/// `sideloadly.io` + `Fzji…` → `https://sideloadly.io/anisette/Fzji…`.
pub fn sideloadly_anisette_url(host: &str, token: &str) -> String {
    let host = host
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_matches('/');
    format!("https://{host}/anisette/{}", token.trim())
}

/// Ensures the per-install device hash file exists under `path`, creating a new
/// random one if the file is missing or malformed. Returns the 64-hex-char hash.
pub fn ensure_device_hash(path: &std::path::Path) -> Result<String, AnisetteError> {
    std::fs::create_dir_all(path)?;
    let hash_path = path.join(DEVICE_HASH_FILE);
    match std::fs::read_to_string(&hash_path) {
        Ok(s) if s.trim().len() == 64 && s.trim().chars().all(|c| c.is_ascii_hexdigit()) => {
            Ok(s.trim().to_string())
        }
        _ => {
            let bytes = rand::random::<[u8; 32]>();
            let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
            std::fs::write(&hash_path, &hex)?;
            Ok(hex)
        }
    }
}

pub struct RemoteAnisetteProviderV1 {
    url: String,
    configuration_path: PathBuf,
    device_hash: Option<String>,
}

impl RemoteAnisetteProviderV1 {
    pub fn new(url: String, configuration_path: PathBuf) -> RemoteAnisetteProviderV1 {
        RemoteAnisetteProviderV1 {
            url: url.trim().trim_end_matches('/').to_string(),
            configuration_path,
            device_hash: None,
        }
    }

    /// Persistent 32-byte (64 hex char) identifier sent as `?u=`. The server keys
    /// the provisioned Mac identity off this, so it must stay stable per install.
    fn device_hash(&mut self) -> Result<String, AnisetteError> {
        if let Some(h) = &self.device_hash {
            return Ok(h.clone());
        }
        std::fs::create_dir_all(&self.configuration_path)?;
        let path = self.configuration_path.join(DEVICE_HASH_FILE);
        let hash = match std::fs::read_to_string(&path) {
            Ok(s) if s.trim().len() == 64 && s.trim().chars().all(|c| c.is_ascii_hexdigit()) => {
                s.trim().to_string()
            }
            _ => {
                let bytes = rand::random::<[u8; 32]>();
                let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
                std::fs::write(&path, &hex)?;
                hex
            }
        };
        self.device_hash = Some(hash.clone());
        Ok(hash)
    }

    fn request_url(&mut self) -> Result<String, AnisetteError> {
        // A plain v1 server takes no `u`; a Sideloadly-style endpoint expects one.
        // Respect a `u` the caller already put in the URL instead of adding a second.
        if self.url.contains("u=") {
            return Ok(self.url.clone());
        }
        let u = self.device_hash()?;
        let sep = if self.url.contains('?') { '&' } else { '?' };
        Ok(format!("{}{sep}u={u}", self.url))
    }

    /// Probes whether `url` answers with v1 anisette JSON, so callers can tell a
    /// v1 server apart from a v3 one.
    pub async fn probe(url: &str, configuration_path: PathBuf) -> bool {
        let mut p = RemoteAnisetteProviderV1::new(url.to_string(), configuration_path);
        p.get_anisette_headers(false).await.is_ok()
    }
}

#[async_trait]
impl AnisetteHeadersProvider for RemoteAnisetteProviderV1 {
    async fn get_anisette_headers(
        &mut self,
        _skip_provisioning: bool,
    ) -> Result<HashMap<String, String>, AnisetteError> {
        let url = self.request_url()?;
        debug!("Fetching v1 anisette from {url}");
        let headers = Client::new()
            .get(&url)
            .header("Accept", "application/json")
            .send()
            .await?
            .error_for_status()?
            .json::<HashMap<String, String>>()
            .await?;

        // A real anisette response always carries the one-time password / machine id.
        if !headers.contains_key("X-Apple-I-MD") && !headers.contains_key("X-Apple-I-MD-M") {
            return Err(AnisetteError::InvalidArgument(format!(
                "{} did not return anisette headers",
                self.url
            )));
        }
        Ok(headers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AnisetteConfiguration, AnisetteHeaders, AnisetteProvider};

    #[test]
    fn builds_sideloadly_url_for_any_instance() {
        assert_eq!(
            sideloadly_anisette_url(DEFAULT_SIDELOADLY_HOST, "Fzji"),
            "https://sideloadly.io/anisette/Fzji"
        );
        // A self-hosted host, with or without a scheme or trailing slash.
        assert_eq!(
            sideloadly_anisette_url("https://ani.example.com/", " Fzji "),
            "https://ani.example.com/anisette/Fzji"
        );
    }

    // Set OMNISETTE_TEST_REMOTE_URL to a v1 (or v3) server to exercise AutoRemoteProvider.
    #[tokio::test]
    #[ignore = "network"]
    async fn auto_remote_detects_and_fetches() {
        let url =
            std::env::var("OMNISETTE_TEST_REMOTE_URL").expect("set OMNISETTE_TEST_REMOTE_URL");
        let dir = std::env::temp_dir().join("omnisette_v1_test");
        let cfg = AnisetteConfiguration::new()
            .set_configuration_path(dir)
            .set_provider(AnisetteProvider::Remote)
            .set_anisette_url_v3(url);
        let mut res = AnisetteHeaders::get_anisette_headers_provider(cfg).unwrap();
        let h = res.provider.get_authentication_headers().await.unwrap();
        assert!(
            h.contains_key("X-Apple-I-MD") || h.contains_key("X-Apple-I-MD-M"),
            "{h:?}"
        );
        println!(
            "OK device-id={:?} srl={:?}",
            h.get("X-Mme-Device-Id"),
            h.get("X-Apple-I-SRL-NO")
        );
    }
}
