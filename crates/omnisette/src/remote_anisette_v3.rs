// Implementing the SideStore Anisette v3 protocol

use std::{
    collections::HashMap,
    fs,
    io::Cursor,
    path::{Path, PathBuf},
};

use crate::ANISETTE_STATE_FILE;
use crate::adi_proxy::LOOKUP_CLIENT_INFO;
use crate::anisette_headers_provider::normalize_client_info;
use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose;
use chrono::{DateTime, SubsecRound, Utc};
use futures_util::{SinkExt, stream::StreamExt};
use log::debug;
use plist::{Data, Dictionary};
use reqwest::{Client, ClientBuilder, RequestBuilder};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::fmt::Write;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use uuid::Uuid;

use crate::{AnisetteError, anisette_headers_provider::AnisetteHeadersProvider};

fn plist_to_string<T: serde::Serialize>(value: &T) -> Result<String, plist::Error> {
    plist_to_buf(value).map(|val| String::from_utf8(val).unwrap())
}

fn plist_to_buf<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, plist::Error> {
    let mut buf: Vec<u8> = Vec::new();
    let writer = Cursor::new(&mut buf);
    plist::to_writer_xml(writer, &value)?;
    Ok(buf)
}

fn bin_serialize<S>(x: &[u8], s: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    s.serialize_bytes(x)
}

fn bin_serialize_opt<S>(x: &Option<Vec<u8>>, s: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    x.clone().map(Data::new).serialize(s)
}

fn bin_deserialize_opt<'de, D>(d: D) -> Result<Option<Vec<u8>>, D::Error>
where
    D: Deserializer<'de>,
{
    let s: Option<Data> = Deserialize::deserialize(d)?;
    Ok(s.map(|i| i.into()))
}

fn bin_deserialize_16<'de, D>(d: D) -> Result<[u8; 16], D::Error>
where
    D: Deserializer<'de>,
{
    let s: Data = Deserialize::deserialize(d)?;
    let s: Vec<u8> = s.into();
    Ok(s.try_into().unwrap())
}

fn encode_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        write!(&mut s, "{:02x}", b).unwrap();
    }
    s
}
fn base64_encode(data: &[u8]) -> String {
    general_purpose::STANDARD.encode(data)
}

fn base64_decode(data: &str) -> Vec<u8> {
    general_purpose::STANDARD.decode(data.trim()).unwrap()
}

#[derive(Deserialize)]
struct AnisetteClientInfo {
    client_info: String,
    user_agent: String,
}

#[derive(Serialize, Deserialize)]
pub struct AnisetteState {
    #[serde(
        serialize_with = "bin_serialize",
        deserialize_with = "bin_deserialize_16"
    )]
    keychain_identifier: [u8; 16],
    #[serde(
        serialize_with = "bin_serialize_opt",
        deserialize_with = "bin_deserialize_opt"
    )]
    adi_pb: Option<Vec<u8>>,
}

impl Default for AnisetteState {
    fn default() -> Self {
        AnisetteState {
            keychain_identifier: rand::random::<[u8; 16]>(),
            adi_pb: None,
        }
    }
}

impl AnisetteState {
    pub fn new() -> AnisetteState {
        AnisetteState::default()
    }

    fn load(configuration_path: &Path) -> Self {
        plist::from_file(configuration_path.join(ANISETTE_STATE_FILE)).unwrap_or_default()
    }

    pub fn is_provisioned(&self) -> bool {
        self.adi_pb.is_some()
    }

    fn md_lu(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(self.keychain_identifier);
        hasher.finalize().into()
    }

    fn device_id(&self) -> String {
        Uuid::from_bytes(self.keychain_identifier).to_string()
    }
}
pub struct AnisetteClient {
    client_info: AnisetteClientInfo,
    url: String,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct ProvisionBodyData {
    header: Dictionary,
    request: Dictionary,
}

#[derive(Debug)]
pub struct AnisetteData {
    machine_id: String,
    one_time_password: String,
    routing_info: String,
    device_description: String,
    user_agent: String,
    local_user_id: String,
    device_unique_identifier: String,
}

impl AnisetteData {
    pub fn get_headers(&self, serial: String) -> HashMap<String, String> {
        let dt: DateTime<Utc> = Utc::now().round_subsecs(0);

        HashMap::from_iter([
            ("User-Agent".into(), self.user_agent.clone()),
            (
                "X-Apple-I-Client-Time".to_string(),
                dt.format("%+").to_string().replace("+00:00", "Z"),
            ),
            ("X-Apple-I-SRL-NO".to_string(), serial),
            ("X-Apple-I-TimeZone".to_string(), "UTC".to_string()),
            ("X-Apple-Locale".to_string(), "en_US".to_string()),
            ("X-Apple-I-MD-RINFO".to_string(), self.routing_info.clone()),
            ("X-Apple-I-MD-LU".to_string(), self.local_user_id.clone()),
            (
                "X-Mme-Device-Id".to_string(),
                self.device_unique_identifier.clone(),
            ),
            ("X-Apple-I-MD".to_string(), self.one_time_password.clone()),
            ("X-Apple-I-MD-M".to_string(), self.machine_id.clone()),
            (
                "X-Mme-Client-Info".to_string(),
                self.device_description.clone(),
            ),
        ])
    }
}

fn make_reqwest() -> Result<Client, AnisetteError> {
    Ok(ClientBuilder::new()
        .http1_title_case_headers()
        .danger_accept_invalid_certs(true) // TODO: pin the apple certificate
        .build()?)
}

impl AnisetteClient {
    pub async fn new(url: String) -> Result<AnisetteClient, AnisetteError> {
        let path = format!("{}/v3/client_info", url);
        let http_client = make_reqwest()?;
        let mut client_info = http_client
            .get(path)
            .send()
            .await?
            .error_for_status()?
            .json::<AnisetteClientInfo>()
            .await?;
        client_info.client_info = normalize_client_info(&client_info.client_info);
        Ok(AnisetteClient { client_info, url })
    }

    fn build_apple_request(
        &self,
        state: &AnisetteState,
        builder: RequestBuilder,
        client_info: &str,
    ) -> RequestBuilder {
        let dt: DateTime<Utc> = Utc::now().round_subsecs(0);

        builder
            .header("X-Mme-Client-Info", client_info)
            .header("User-Agent", &self.client_info.user_agent)
            .header("Content-Type", "text/x-xml-plist")
            .header("X-Apple-I-MD-LU", encode_hex(&state.md_lu()))
            .header("X-Mme-Device-Id", state.device_id())
            .header("X-Apple-I-Client-Time", dt.format("%+").to_string())
            .header("X-Apple-I-TimeZone", "UTC")
            .header("X-Apple-Locale", "en_US")
    }

    pub async fn get_headers(&self, state: &AnisetteState) -> Result<AnisetteData, AnisetteError> {
        let path = format!("{}/v3/get_headers", self.url);
        let http_client = make_reqwest()?;

        #[derive(Serialize)]
        struct GetHeadersBody {
            identifier: String,
            adi_pb: String,
        }
        let body = GetHeadersBody {
            identifier: base64_encode(&state.keychain_identifier),
            adi_pb: base64_encode(
                state
                    .adi_pb
                    .as_ref()
                    .ok_or(AnisetteError::AnisetteNotProvisioned)?,
            ),
        };

        #[derive(Deserialize)]
        #[serde(tag = "result")]
        enum AnisetteHeaders {
            GetHeadersError {
                message: String,
            },
            Headers {
                #[serde(rename = "X-Apple-I-MD-M")]
                machine_id: String,
                #[serde(rename = "X-Apple-I-MD")]
                one_time_password: String,
                #[serde(rename = "X-Apple-I-MD-RINFO")]
                routing_info: String,
            },
        }

        let headers = http_client
            .post(path)
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json::<AnisetteHeaders>()
            .await?;
        match headers {
            AnisetteHeaders::GetHeadersError { message } => {
                if message.contains("-45061") {
                    Err(AnisetteError::AnisetteNotProvisioned)
                } else {
                    Err(anyhow::anyhow!("Anisette server error: {}", message).into())
                }
            }
            AnisetteHeaders::Headers {
                machine_id,
                one_time_password,
                routing_info,
            } => Ok(AnisetteData {
                machine_id,
                one_time_password,
                routing_info,
                device_description: self.client_info.client_info.clone(),
                user_agent: self.client_info.user_agent.clone(),
                local_user_id: encode_hex(&state.md_lu()),
                device_unique_identifier: state.device_id(),
            }),
        }
    }

    pub async fn provision(&self, state: &mut AnisetteState) -> Result<(), AnisetteError> {
        debug!("Provisioning Anisette");
        let http_client = make_reqwest()?;
        let resp = self
            .build_apple_request(
                state,
                http_client.get("https://gsa.apple.com/grandslam/GsService2/lookup"),
                LOOKUP_CLIENT_INFO,
            )
            .send()
            .await?;
        let text = resp.error_for_status()?.text().await?;

        let protocol_val = plist::Value::from_reader(Cursor::new(text.as_str()))?;
        let urls = protocol_val
            .as_dictionary()
            .unwrap()
            .get("urls")
            .unwrap()
            .as_dictionary()
            .unwrap();

        let start_provisioning_url = urls
            .get("midStartProvisioning")
            .unwrap()
            .as_string()
            .unwrap();
        let end_provisioning_url = urls
            .get("midFinishProvisioning")
            .unwrap()
            .as_string()
            .unwrap();
        debug!(
            "Got provisioning urls: {} and {}",
            start_provisioning_url, end_provisioning_url
        );

        let provision_ws_url = format!("{}/v3/provisioning_session", self.url)
            .replacen("https://", "wss://", 1)
            .replacen("http://", "ws://", 1);
        let (mut connection, _) = connect_async(&provision_ws_url).await?;

        #[derive(Deserialize)]
        #[serde(tag = "result")]
        enum ProvisionInput {
            GiveIdentifier,
            GiveStartProvisioningData,
            GiveEndProvisioningData {
                #[allow(dead_code)] // it's not even dead, rust just has problems
                cpim: String,
            },
            ProvisioningSuccess {
                #[allow(dead_code)] // it's not even dead, rust just has problems
                adi_pb: String,
            },
        }

        loop {
            let Some(Ok(data)) = connection.next().await else {
                continue;
            };
            if data.is_text() {
                let txt = data.to_text().unwrap();
                let msg: ProvisionInput = serde_json::from_str(txt)?;
                match msg {
                    ProvisionInput::GiveIdentifier => {
                        #[derive(Serialize)]
                        struct Identifier {
                            identifier: String, // base64
                        }
                        let identifier = Identifier {
                            identifier: base64_encode(&state.keychain_identifier),
                        };
                        connection
                            .send(Message::Text(serde_json::to_string(&identifier)?.into()))
                            .await?;
                    }
                    ProvisionInput::GiveStartProvisioningData => {
                        let http_client = make_reqwest()?;
                        let body_data = ProvisionBodyData {
                            header: Dictionary::new(),
                            request: Dictionary::new(),
                        };
                        let resp = self
                            .build_apple_request(
                                state,
                                http_client.post(start_provisioning_url),
                                &self.client_info.client_info,
                            )
                            .body(plist_to_string(&body_data)?)
                            .send()
                            .await?;
                        let text = resp.error_for_status()?.text().await?;

                        let protocol_val = plist::Value::from_reader(Cursor::new(text.as_str()))?;
                        let spim = protocol_val
                            .as_dictionary()
                            .unwrap()
                            .get("Response")
                            .unwrap()
                            .as_dictionary()
                            .unwrap()
                            .get("spim")
                            .unwrap()
                            .as_string()
                            .unwrap();

                        debug!("GiveStartProvisioningData");
                        #[derive(Serialize)]
                        struct Spim {
                            spim: String, // base64
                        }
                        let spim = Spim {
                            spim: spim.to_string(),
                        };
                        connection
                            .send(Message::Text(serde_json::to_string(&spim)?.into()))
                            .await?;
                    }
                    ProvisionInput::GiveEndProvisioningData { cpim } => {
                        let http_client = make_reqwest()?;
                        let body_data = ProvisionBodyData {
                            header: Dictionary::new(),
                            request: Dictionary::from_iter([("cpim", cpim)]),
                        };
                        let resp = self
                            .build_apple_request(
                                state,
                                http_client.post(end_provisioning_url),
                                &self.client_info.client_info,
                            )
                            .body(plist_to_string(&body_data)?)
                            .send()
                            .await?;
                        let text = resp.error_for_status()?.text().await?;

                        let protocol_val = plist::Value::from_reader(Cursor::new(text.as_str()))?;
                        let response = protocol_val
                            .as_dictionary()
                            .unwrap()
                            .get("Response")
                            .unwrap()
                            .as_dictionary()
                            .unwrap();

                        debug!("GiveEndProvisioningData");

                        #[derive(Serialize)]
                        struct EndProvisioning<'t> {
                            ptm: &'t str,
                            tk: &'t str,
                        }
                        let end_provisioning = EndProvisioning {
                            ptm: response.get("ptm").unwrap().as_string().unwrap(),
                            tk: response.get("tk").unwrap().as_string().unwrap(),
                        };
                        connection
                            .send(Message::Text(
                                serde_json::to_string(&end_provisioning)?.into(),
                            ))
                            .await?;
                    }
                    ProvisionInput::ProvisioningSuccess { adi_pb } => {
                        debug!("ProvisioningSuccess");
                        state.adi_pb = Some(base64_decode(&adi_pb));
                        connection.close(None).await?;
                        break;
                    }
                }
            } else if data.is_close() {
                break;
            }
        }

        Ok(())
    }
}

pub struct RemoteAnisetteProviderV3 {
    client_url: String,
    client: Option<AnisetteClient>,
    pub state: Option<AnisetteState>,
    configuration_path: PathBuf,
    serial: String,
}

impl RemoteAnisetteProviderV3 {
    pub fn new(
        url: String,
        configuration_path: PathBuf,
        serial: String,
    ) -> RemoteAnisetteProviderV3 {
        RemoteAnisetteProviderV3 {
            client_url: url.trim().trim_end_matches('/').to_string(),
            client: None,
            state: None,
            configuration_path,
            serial,
        }
    }
}

#[async_trait]
impl AnisetteHeadersProvider for RemoteAnisetteProviderV3 {
    async fn get_anisette_headers(
        &mut self,
        _skip_provisioning: bool,
    ) -> Result<HashMap<String, String>, AnisetteError> {
        if self.client.is_none() {
            self.client = Some(AnisetteClient::new(self.client_url.clone()).await?);
        }
        let client = self.client.as_ref().unwrap();

        fs::create_dir_all(&self.configuration_path)?;

        let config_path = self.configuration_path.join(ANISETTE_STATE_FILE);
        if self.state.is_none() {
            self.state = Some(AnisetteState::load(&self.configuration_path));
        }

        let state = self.state.as_mut().unwrap();
        if !state.is_provisioned() {
            client.provision(state).await?;
            plist::to_file_xml(&config_path, state)?;
        }
        let data = match client.get_headers(state).await {
            Ok(data) => data,
            Err(err) => {
                if matches!(err, AnisetteError::AnisetteNotProvisioned) {
                    state.adi_pb = None;
                    client.provision(state).await?;
                    plist::to_file_xml(config_path, state)?;
                    client.get_headers(state).await?
                } else {
                    return Err(err);
                }
            }
        };
        Ok(data.get_headers(self.serial.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_state_is_not_reused_and_current_state_survives_refresh() {
        let path = std::env::temp_dir().join(format!("jimpactor-state-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        let state = AnisetteState {
            keychain_identifier: [7; 16],
            adi_pb: Some(vec![8; 16]),
        };
        plist::to_file_xml(path.join("state.plist"), &state).unwrap();
        assert!(!AnisetteState::load(&path).is_provisioned());
        plist::to_file_xml(path.join(ANISETTE_STATE_FILE), &state).unwrap();
        let loaded = AnisetteState::load(&path);
        assert_eq!(loaded.keychain_identifier, state.keychain_identifier);
        assert_eq!(loaded.adi_pb, state.adi_pb);
        fs::remove_dir_all(path).unwrap();
    }

    #[tokio::test]
    async fn regression_server_identity_used_for_provisioning_and_auth_headers() {
        check_server_identity(
            "<MacBookPro13,3> <Mac OS X;10.12.6;16G29> <com.apple.AuthKit/1 (com.apple.akd/1.0)>",
        )
        .await;
    }

    #[tokio::test]
    async fn legacy_xcode_identity_is_corrected_before_provisioning_and_authentication() {
        check_server_identity("<MacBookPro13,3> <Mac OS X;10.12.6;16G29> <com.apple.AuthKit/1 (com.apple.dt.Xcode/3594.4.19)>").await;
    }

    async fn check_server_identity(client_info: &str) {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::time::Duration;

        const MAC_CLIENT_INFO: &str =
            "<MacBookPro13,3> <Mac OS X;10.12.6;16G29> <com.apple.AuthKit/1 (com.apple.akd/1.0)>";
        const USER_AGENT: &str = "akd/1.0 CFNetwork/808.3 Darwin/16.6.0";

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let client_info = client_info.to_owned();
        let server = std::thread::spawn(move || {
            let responses = [
                (
                    "GET /v3/client_info ",
                    serde_json::json!({
                        "client_info": client_info,
                        "user_agent": USER_AGENT,
                    }),
                ),
                (
                    "POST /v3/get_headers ",
                    serde_json::json!({
                        "result": "Headers",
                        "X-Apple-I-MD-M": "test-machine",
                        "X-Apple-I-MD": "test-otp",
                        "X-Apple-I-MD-RINFO": "17106176",
                    }),
                ),
            ];
            for (expected_request, response) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buffer = [0; 1024];
                loop {
                    let count = stream.read(&mut buffer).unwrap();
                    assert_ne!(count, 0, "request closed before its body was received");
                    request.extend_from_slice(&buffer[..count]);
                    if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&request[..end]);
                        let content_length = headers
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if request.len() >= end + 4 + content_length {
                            break;
                        }
                    }
                }
                assert!(String::from_utf8_lossy(&request).starts_with(expected_request));
                let body = response.to_string();
                write!(stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(), body
                ).unwrap();
            }
        });

        let client = AnisetteClient::new(url).await.unwrap();
        let state = AnisetteState {
            keychain_identifier: [1; 16],
            adi_pb: Some(vec![2; 16]),
        };
        let request = client
            .build_apple_request(
                &state,
                reqwest::Client::new().post("http://localhost/provision"),
                &client.client_info.client_info,
            )
            .build()
            .unwrap();
        assert_eq!(request.headers()["X-Mme-Client-Info"], MAC_CLIENT_INFO);
        assert_eq!(request.headers()["User-Agent"], USER_AGENT);

        let headers = client
            .get_headers(&state)
            .await
            .unwrap()
            .get_headers("0".into());
        assert_eq!(headers["X-Mme-Client-Info"], MAC_CLIENT_INFO);
        assert_eq!(headers["User-Agent"], USER_AGENT);
        assert_eq!(headers["X-Apple-I-MD-M"], "test-machine");
        assert_eq!(headers["X-Apple-I-MD"], "test-otp");
        server.join().unwrap();
    }

    #[tokio::test]
    async fn reports_http_failure_before_attempting_to_decode_html() {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                .unwrap();
            let mut reader = BufReader::new(&mut stream);
            loop {
                let mut line = String::new();
                assert!(reader.read_line(&mut line).unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
            }
            drop(reader);
            stream.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Type: text/html\r\nContent-Length: 6\r\nConnection: close\r\n\r\n<html>").unwrap();
        });
        let error = match AnisetteClient::new(format!("http://{address}")).await {
            Ok(_) => panic!("expected HTTP error"),
            Err(error) => error,
        };
        assert!(
            matches!(error, AnisetteError::ReqwestError(ref error) if error.status().map(|status| status.as_u16()) == Some(503))
        );
        server.join().unwrap();
    }
}
