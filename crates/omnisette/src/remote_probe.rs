//! Read-only metadata probes for remote anisette services. No Apple login or
//! v3 provisioning is performed, and authentication headers are never retained.

use std::path::Path;
use std::time::{Duration, Instant};

use reqwest::{Client, Url};
use serde_json::Value;

use crate::AnisetteError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Local,
    V1,
    V3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportedIdentity {
    Mac,
    Windows,
    Linux,
    AppleMobile,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct ServerInfo {
    pub protocol: Protocol,
    pub elapsed: Duration,
    pub identity: ReportedIdentity,
    pub model: Option<String>,
    pub operating_system: Option<String>,
    pub client_info: Option<String>,
    pub user_agent: Option<String>,
    pub server_software: Option<String>,
}

fn field(json: &Value, name: &str) -> Option<String> {
    json.as_object()?
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))?
        .1
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.chars().take(512).collect())
}

impl ServerInfo {
    /// Describes a local provider without retaining authentication headers.
    pub fn from_local_headers(
        headers: &std::collections::HashMap<String, String>,
        elapsed: Duration,
    ) -> Self {
        let json = serde_json::to_value(headers).unwrap_or_default();
        Self::from_response(Protocol::Local, &json, None, elapsed)
    }

    fn from_response(
        protocol: Protocol,
        json: &Value,
        server_software: Option<String>,
        elapsed: Duration,
    ) -> Self {
        let client_info = field(json, "client_info").or_else(|| field(json, "X-Mme-Client-Info"));
        let parts: Vec<&str> = client_info
            .as_deref()
            .unwrap_or_default()
            .split('<')
            .skip(1)
            .filter_map(|part| part.split_once('>').map(|(value, _)| value.trim()))
            .collect();
        let model = parts
            .first()
            .filter(|part| !part.is_empty())
            .map(|s| s.to_string());
        let operating_system = parts
            .get(1)
            .filter(|part| !part.is_empty())
            .map(|s| s.to_string());
        let description = client_info
            .as_deref()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let identity = if model.as_deref().is_some_and(|model| {
            let model = model.to_ascii_lowercase();
            model.starts_with("mac") || model.starts_with("imac")
        }) || description.contains("macos")
            || description.contains("mac os x")
        {
            ReportedIdentity::Mac
        } else if description.contains("windows") {
            ReportedIdentity::Windows
        } else if description.contains("linux") || description.contains("android") {
            ReportedIdentity::Linux
        } else if description.contains("iphone") || description.contains("ipad") {
            ReportedIdentity::AppleMobile
        } else {
            ReportedIdentity::Unknown
        };
        Self {
            protocol,
            elapsed,
            identity,
            model,
            operating_system,
            client_info,
            user_agent: field(json, "user_agent").or_else(|| field(json, "User-Agent")),
            server_software,
        }
    }
}

async fn get(client: &Client, url: Url) -> Result<(Value, Option<String>), AnisetteError> {
    let response = client
        .get(url)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|error| error.without_url())?
        .error_for_status()
        .map_err(|error| error.without_url())?;
    let software = response
        .headers()
        .get("server")
        .and_then(|s| s.to_str().ok())
        .map(|s| s.chars().take(128).collect());
    let json = response.json().await.map_err(|error| error.without_url())?;
    Ok((json, software))
}

/// Detects v3 client metadata first, falling back to v1 headers. A reported Mac identity is a claim
/// made by the service; it cannot attest to the hardware hosting that service.
pub async fn probe_server(address: &str, data_path: &Path) -> Result<ServerInfo, AnisetteError> {
    let mut url = Url::parse(address)
        .map_err(|_| AnisetteError::InvalidArgument("invalid server URL".into()))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(AnisetteError::InvalidArgument(
            "expected an HTTP or HTTPS server".into(),
        ));
    }
    let client = Client::builder().timeout(Duration::from_secs(6)).build()?;
    let mut v1_url = url.clone();
    url.set_path(&format!(
        "{}/v3/client_info",
        url.path().trim_end_matches('/')
    ));
    url.set_query(None);
    let start = Instant::now();
    let v3_error = match get(&client, url).await {
        Ok((json, software)) if field(&json, "client_info").is_some() => {
            return Ok(ServerInfo::from_response(
                Protocol::V3,
                &json,
                software,
                start.elapsed(),
            ));
        }
        Ok(_) => "missing client_info".to_string(),
        Err(error) => error.to_string(),
    };
    if !v1_url.query_pairs().any(|(key, _)| key == "u") {
        let hash = crate::remote_anisette_v1::ensure_device_hash(data_path)?;
        v1_url.query_pairs_mut().append_pair("u", &hash);
    }
    let start = Instant::now();
    match get(&client, v1_url).await {
        Ok((json, software))
            if field(&json, "X-Apple-I-MD").is_some()
                && field(&json, "X-Apple-I-MD-M").is_some() =>
        {
            return Ok(ServerInfo::from_response(
                Protocol::V1,
                &json,
                software,
                start.elapsed(),
            ));
        }
        Ok(_) => Err(AnisetteError::InvalidArgument(format!(
            "No supported anisette response. v3: {v3_error}; v1: missing anisette headers"
        ))),
        Err(error) => Err(AnisetteError::InvalidArgument(format!(
            "No supported anisette response. v3: {v3_error}; v1: {error}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    fn server(responses: Vec<Value>) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let task = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut requests = Vec::new();
            for response in responses {
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(
                                Instant::now() < deadline,
                                "probe did not make the expected request"
                            );
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("{error}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                reader.read_line(&mut request).unwrap();
                requests.push(request);
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                        break;
                    }
                }
                let body = response.to_string();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nServer: test-server\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
            }
            requests
        });
        (address, task)
    }

    #[tokio::test]
    async fn probes_v1_without_overwriting_an_existing_identity() {
        let (address, task) = server(vec![
            json!({}),
            json!({
                "X-Apple-I-MD": "otp", "X-Apple-I-MD-M": "machine",
                "X-MMe-Client-Info": "<iMac21,1> <macOS;14.5> <AuthKit>"
            }),
        ]);
        let path = std::env::temp_dir().join(format!("jimpactor-probe-{}", uuid::Uuid::new_v4()));
        let info = probe_server(
            &format!("{address}/anisette/test?u=preserved&keep=yes"),
            &path,
        )
        .await
        .unwrap();
        assert_eq!(info.protocol, Protocol::V1);
        assert_eq!(info.identity, ReportedIdentity::Mac);
        assert_eq!(info.server_software.as_deref(), Some("test-server"));
        let requests = task.join().unwrap();
        assert!(requests[0].starts_with("GET /anisette/test/v3/client_info HTTP/1.1"));
        assert!(requests[1].contains("u=preserved&keep=yes"));
        assert_eq!(requests[1].matches("u=").count(), 1);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn probes_v3_metadata_without_provisioning_or_claiming_host_hardware() {
        let (address, task) = server(vec![json!({
            "client_info":"<Unknown> <Darwin> <AuthKit>", "user_agent":"test-agent"
        })]);
        let path = std::env::temp_dir().join(format!("jimpactor-probe-{}", uuid::Uuid::new_v4()));
        let info = probe_server(&format!("{address}?u=preserved"), &path)
            .await
            .unwrap();
        assert_eq!(info.protocol, Protocol::V3);
        assert_eq!(info.identity, ReportedIdentity::Unknown);
        assert_eq!(info.user_agent.as_deref(), Some("test-agent"));
        let requests = task.join().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with("GET /v3/client_info HTTP/1.1"));
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn rejects_partial_headers_and_non_anisette_json() {
        let (address, task) = server(vec![
            json!({"client_info":""}),
            json!({"X-Apple-I-MD":"otp"}),
        ]);
        let path = std::env::temp_dir();
        let result = probe_server(&format!("{address}?u=preserved"), &path).await;
        assert!(result.is_err());
        assert_eq!(task.join().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn authentication_falls_back_to_v1_and_remembers_the_protocol() {
        use crate::{AutoRemoteProvider, anisette_headers_provider::AnisetteHeadersProvider};

        let headers = json!({"X-Apple-I-MD": "otp", "X-Apple-I-MD-M": "machine"});
        let (address, task) = server(vec![json!({}), headers.clone(), headers]);
        let path = std::env::temp_dir().join(format!("jimpactor-auth-{}", uuid::Uuid::new_v4()));
        let mut provider = AutoRemoteProvider::new(address, path.clone(), "0".into());
        for _ in 0..2 {
            let headers = provider.get_anisette_headers(false).await.unwrap();
            assert_eq!(headers["X-Apple-I-MD"], "otp");
        }
        let requests = task.join().unwrap();
        assert_eq!(requests.len(), 3);
        assert!(requests[0].starts_with("GET /v3/client_info HTTP/1.1"));
        assert!(requests[1].starts_with("GET /?u="));
        assert!(requests[2].starts_with("GET /?u="));
        std::fs::remove_dir_all(path).unwrap();
    }

    #[tokio::test]
    async fn authentication_prefers_v3_and_reuses_it_without_trying_v1() {
        use crate::{AutoRemoteProvider, anisette_headers_provider::AnisetteHeadersProvider};

        let headers = json!({
            "result": "Headers", "X-Apple-I-MD": "otp", "X-Apple-I-MD-M": "machine",
            "X-Apple-I-MD-RINFO": "1"
        });
        let (address, task) = server(vec![
            json!({"client_info": "<Mac> <macOS> <AuthKit>", "user_agent": "test"}),
            headers.clone(),
            headers,
        ]);
        let path = std::env::temp_dir().join(format!("jimpactor-auth-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        let mut state = plist::Dictionary::new();
        state.insert(
            "keychain_identifier".into(),
            plist::Value::Data(vec![0; 16]),
        );
        state.insert("adi_pb".into(), plist::Value::Data(vec![1]));
        plist::to_file_xml(path.join("state.plist"), &state).unwrap();
        let mut provider = AutoRemoteProvider::new(address, path.clone(), "0".into());
        for _ in 0..2 {
            let headers = provider.get_anisette_headers(false).await.unwrap();
            assert_eq!(headers["X-Apple-I-MD"], "otp");
        }
        let requests = task.join().unwrap();
        assert_eq!(requests.len(), 3);
        assert!(requests[0].starts_with("GET /v3/client_info HTTP/1.1"));
        assert!(requests[1].starts_with("POST /v3/get_headers HTTP/1.1"));
        assert!(requests[2].starts_with("POST /v3/get_headers HTTP/1.1"));
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn local_probe_reports_metadata_without_retaining_authentication_headers() {
        let info = ServerInfo::from_local_headers(
            &std::collections::HashMap::from([
                (
                    "X-Mme-Client-Info".into(),
                    "<MacBookPro18,3> <macOS;14.5> <AuthKit>".into(),
                ),
                ("X-Apple-I-MD".into(), "private-otp".into()),
            ]),
            Duration::ZERO,
        );
        assert_eq!(info.protocol, Protocol::Local);
        assert_eq!(info.identity, ReportedIdentity::Mac);
        assert_eq!(info.model.as_deref(), Some("MacBookPro18,3"));
        assert!(!format!("{info:?}").contains("private-otp"));
    }

    #[test]
    fn identifies_reported_mac_model_and_os_without_retaining_authentication_data() {
        let info = ServerInfo::from_response(
            Protocol::V1,
            &json!({
                "X-MMe-Client-Info": "<MacBookPro18,3> <macOS;14.5;23F79> <com.apple.AuthKit/1>",
                "X-Apple-I-MD": "private-one-time-password",
                "X-Apple-I-MD-M": "private-machine-identity"
            }),
            None,
            Duration::ZERO,
        );
        assert_eq!(info.identity, ReportedIdentity::Mac);
        assert_eq!(info.model.as_deref(), Some("MacBookPro18,3"));
        assert_eq!(info.operating_system.as_deref(), Some("macOS;14.5;23F79"));
        assert!(!format!("{info:?}").contains("private-"));
    }

    #[test]
    fn distinguishes_non_mac_and_missing_identities() {
        for (description, expected) in [
            ("<PC> <Windows;10> <AuthKit>", ReportedIdentity::Windows),
            ("<PC> <Linux> <AuthKit>", ReportedIdentity::Linux),
            (
                "<iPhone15,2> <iOS;17> <AuthKit>",
                ReportedIdentity::AppleMobile,
            ),
            ("", ReportedIdentity::Unknown),
            ("<Unknown> <Darwin> <AuthKit>", ReportedIdentity::Unknown),
        ] {
            let info = ServerInfo::from_response(
                Protocol::V3,
                &json!({"client_info": description}),
                Some("nginx".into()),
                Duration::ZERO,
            );
            assert_eq!(info.identity, expected, "{description}");
        }
    }
}
