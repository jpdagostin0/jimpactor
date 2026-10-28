use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use futures::future::BoxFuture;
use p256::{
    ecdsa::{Signature, SigningKey, signature::Verifier},
    pkcs8::EncodePrivateKey,
};
use plist::{Dictionary, Value};
use reqwest::{Method, header::HeaderMap};
use serde_json::{Value as Json, json};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
use xkit::{
    Certificate, Entitlement, Entitlements, Error, Mobileprovision, Result, SigningInfo,
    developer_services::{
        ASCJWTGenerator, ASCKey, DeveloperAPIAuthData, DeveloperAPIClient,
        DeveloperServicesAPIVersionLegacy as Legacy, DeveloperServicesClient,
        DeveloperServicesLoginToken, DeveloperServicesPlatform, XcodeAuthData, next_link,
    },
    http::{HTTPClientProtocol, HTTPRequest, HTTPResponse},
    utilities::{
        DirectoryStorage, KeyValueSigningInfoManager, KeyValueStorage, MemoryKeyValueStorage,
        ProvisioningIdentifiers as IDs, SigningInfoManager, ZIPCompressor,
    },
};

#[derive(Default)]
struct MockHTTP {
    requests: Mutex<Vec<HTTPRequest>>,
    responses: Mutex<VecDeque<HTTPResponse>>,
}
impl MockHTTP {
    fn json(responses: Vec<Json>) -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::default(),
            responses: Mutex::new(
                responses
                    .into_iter()
                    .map(|v| HTTPResponse {
                        status: 200,
                        headers: HeaderMap::new(),
                        body: serde_json::to_vec(&v).unwrap(),
                    })
                    .collect(),
            ),
        })
    }
    fn done(&self) {
        assert!(
            self.responses.lock().unwrap().is_empty(),
            "Not all mock responses were consumed"
        );
    }
}
impl HTTPClientProtocol for MockHTTP {
    fn send(&self, request: HTTPRequest) -> BoxFuture<'_, Result<HTTPResponse>> {
        self.requests.lock().unwrap().push(request);
        Box::pin(async {
            Ok(self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("Unexpected HTTP request"))
        })
    }
}

fn token() -> DeveloperServicesLoginToken {
    DeveloperServicesLoginToken {
        adsid: "123456".into(),
        token: "secret-token".into(),
        expiry: chrono::DateTime::from_timestamp(2_000_000_000, 0).unwrap(),
    }
}
fn client(http: Arc<MockHTTP>) -> DeveloperAPIClient {
    DeveloperAPIClient::with_http(
        DeveloperAPIAuthData::Xcode(XcodeAuthData {
            login_token: token(),
            team_id: "TEAM123456".into(),
        }),
        Some(Arc::new(HeaderMap::new())),
        http,
    )
    .unwrap()
}
fn cert() -> Certificate {
    let mut params = rcgen::CertificateParams::new(Vec::new());
    params.serial_number = Some(rcgen::SerialNumber::from_slice(&[0x03, 0xab]));
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "Apple Development: Test");
    params
        .distinguished_name
        .push(rcgen::DnType::OrganizationalUnitName, "TEAM123456");
    let raw = rcgen::Certificate::from_params(params)
        .unwrap()
        .serialize_der()
        .unwrap();
    Certificate::from_der(raw).unwrap()
}
fn signing_info() -> SigningInfo {
    SigningInfo {
        certificate: cert(),
        private_key: xkit::PrivateKey {
            data: b"test-key".to_vec().into(),
        },
    }
}
fn write_app(path: &std::path::Path, bundle_id: &str) {
    std::fs::create_dir_all(path).unwrap();
    let info = dictionary([
        ("CFBundleIdentifier".into(), bundle_id.into()),
        ("CFBundleExecutable".into(), "Test".into()),
        ("CFBundleName".into(), "Test".into()),
        (
            "CFBundlePackageType".into(),
            if path.extension().is_some_and(|e| e == "appex") {
                "XPC!".into()
            } else {
                "APPL".into()
            },
        ),
        ("CFBundleVersion".into(), "1".into()),
    ]);
    plist::to_file_xml(path.join("Info.plist"), &info).unwrap();
    std::fs::write(
        path.join("Test"),
        include_bytes!("fixtures/minimal-arm64.macho"),
    )
    .unwrap();
}
fn xml(value: &impl serde::Serialize) -> Vec<u8> {
    let mut bytes = Vec::new();
    plist::to_writer_xml(&mut bytes, value).unwrap();
    bytes
}
fn dictionary(values: impl IntoIterator<Item = (String, Value)>) -> Dictionary {
    values.into_iter().collect()
}
fn profile_bytes(certificate: &Certificate, bundle_id: &str) -> Vec<u8> {
    let mut profile = Dictionary::new();
    profile.insert("Name".into(), "Test profile".into());
    profile.insert("UUID".into(), "PROFILE-UUID".into());
    profile.insert(
        "TeamIdentifier".into(),
        Value::Array(vec!["TEAM123456".into()]),
    );
    profile.insert(
        "CreationDate".into(),
        Value::Date(std::time::UNIX_EPOCH.into()),
    );
    profile.insert(
        "ExpirationDate".into(),
        Value::Date((std::time::UNIX_EPOCH + std::time::Duration::from_secs(2_000_000_000)).into()),
    );
    profile.insert(
        "DeveloperCertificates".into(),
        Value::Array(vec![Value::Data(certificate.data().to_vec())]),
    );
    let mut entitlements = Entitlements::default();
    entitlements.update("TEAM123456", bundle_id).unwrap();
    profile.insert("Entitlements".into(), Value::Dictionary(entitlements.0));
    xml(&profile)
}
fn resource(id: &str, kind: &str, attributes: Json) -> Json {
    json!({"id":id,"type":kind,"attributes":attributes})
}

#[test]
fn entitlement_rewrites_preserve_unknown_values_and_free_filter_removes_paid_values() {
    let mut entitlements = Entitlements::new(vec![
        Entitlement::ApplicationIdentifier("OLD.app".into()),
        Entitlement::KeychainAccessGroups(vec!["OLD.*".into()]),
        Entitlement::AppGroups(vec!["group.test".into()]),
        Entitlement::HealthKit(true),
    ])
    .unwrap();
    let custom = Value::Array(vec![Value::Data(vec![1, 2, 3]), Value::Boolean(true)]);
    entitlements
        .0
        .insert("unknown.entitlement".into(), custom.clone());
    entitlements
        .update("TEAM123456", "com.example.app")
        .unwrap();
    assert_eq!(
        entitlements.0["application-identifier"].as_string(),
        Some("TEAM123456.com.example.app")
    );
    assert_eq!(entitlements.0["unknown.entitlement"], custom);
    assert_eq!(
        Entitlements::from_bytes(&entitlements.to_xml().unwrap()).unwrap(),
        entitlements
    );
    let free = entitlements.for_free_team().unwrap();
    assert!(!free.0.contains_key("unknown.entitlement"));
    assert!(!free.0.contains_key("com.apple.security.application-groups"));
    assert_eq!(
        free.0["com.apple.developer.healthkit"].as_boolean(),
        Some(true)
    );
    assert!(free.0.contains_key("application-identifier"));
    entitlements
        .set_entitlements(vec![Entitlement::GetTaskAllow(true)])
        .unwrap();
    assert!(!entitlements.0.contains_key("application-identifier"));
    assert_eq!(entitlements.0["unknown.entitlement"], custom);
}

#[test]
fn entitlement_capability_settings_match_developer_api() {
    let cap = Entitlement::AppleSignIn(vec!["Default".into()])
        .capability()
        .unwrap();
    assert_eq!(cap.capability_type, "APPLE_ID_AUTH");
    assert_eq!(cap.settings[0]["options"][0]["key"], "PRIMARY_APP_CONSENT");
    assert!(!cap.is_free);
    let cap = Entitlement::DataProtection(xkit::entitlements::DataProtection::UntilFirstAuth)
        .capability()
        .unwrap();
    assert!(cap.is_free);
    assert_eq!(
        cap.settings[0]["options"][0]["key"],
        "PROTECTED_UNTIL_FIRST_USER_AUTH"
    );
}

#[test]
fn identifiers_can_be_resigned_without_stacking_prefixes() {
    assert_eq!(
        IDs::identifier(&IDs::sanitize("XTL-OLD.com.example.app"), "a1b2c3d4-0000"),
        "XTL-A1B2C3D4.com.example.app"
    );
    assert_eq!(
        IDs::group_id(
            &IDs::sanitize_group("group.XTL-OLD.com.example.group"),
            "TEAM"
        ),
        "group.XTL-TEAM.com.example.group"
    );
    assert_eq!(IDs::app_name("com.example.app"), "XTool com example app");
    assert_eq!(IDs::sanitize("com.example.app"), "com.example.app");
}

#[test]
fn certificate_serials_compare_numerically_and_keep_leading_nibbles() {
    let cert = cert();
    assert_eq!(cert.serial_number(), "03AB");
    assert!(cert.has_serial_number("0003ab"));
    assert!(cert.has_serial_number("3AB"));
    assert!(!cert.has_serial_number("3AC"));
    assert_eq!(cert.team_id().unwrap(), "TEAM123456");
    assert_eq!(
        cert.developer_identity().unwrap(),
        "Apple Development: Test"
    );
    let data = xml(&cert);
    assert_eq!(
        plist::from_bytes::<Certificate>(&data).unwrap().data(),
        cert.data()
    );
}

#[test]
fn profiles_parse_cms_wrapped_xml_and_binary_plists() {
    let cert = cert();
    let raw = profile_bytes(&cert, "com.example.app");
    let mut cms = vec![0x30, 0x82, 0, 0];
    cms.extend_from_slice(&raw);
    cms.extend_from_slice(&[0, 1, 2]);
    let profile = Mobileprovision::from_bytes(cms.clone()).unwrap();
    assert_eq!(profile.data(), cms);
    assert_eq!(profile.digest().uuid, "PROFILE-UUID");
    assert!(profile.digest().devices.is_empty());
    assert_eq!(profile.digest().certificates[0].data(), cert.data());
    let value: Value = plist::from_bytes(&raw).unwrap();
    let mut binary = Vec::new();
    plist::to_writer_binary(&mut binary, &value).unwrap();
    assert_eq!(
        Mobileprovision::from_bytes(binary).unwrap().digest().name,
        "Test profile"
    );
    assert!(Mobileprovision::from_bytes(b"</plist><plist".to_vec()).is_err());
}

#[test]
fn persistent_and_memory_stores_support_replacement_removal_and_signing_info() {
    let temp = tempfile::tempdir().unwrap();
    let disk = DirectoryStorage::new(temp.path());
    for store in [
        &disk as &dyn KeyValueStorage,
        &MemoryKeyValueStorage::default(),
    ] {
        assert!(store.data("missing").unwrap().is_none());
        store.set_string("nested/key", Some("first")).unwrap();
        store.set_string("nested/key", Some("second")).unwrap();
        assert_eq!(
            store.string("nested/key").unwrap().as_deref(),
            Some("second")
        );
        store.set_data("nested/key", None).unwrap();
        store.set_data("nested/key", None).unwrap();
        assert!(store.data("nested/key").unwrap().is_none());
    }
    assert!(disk.set_string("../escape", Some("bad")).is_err());
    assert!(disk.set_string("C:/escape", Some("bad")).is_err());
    let manager = KeyValueSigningInfoManager {
        storage: Arc::new(disk),
    };
    let info = signing_info();
    manager.set_info("identity", Some(&info)).unwrap();
    assert_eq!(
        manager
            .info("identity")
            .unwrap()
            .unwrap()
            .certificate
            .data(),
        info.certificate.data()
    );
    manager.set_info("identity", None).unwrap();
    assert!(manager.info("identity").unwrap().is_none());
}

#[test]
fn legacy_requests_encode_protocol_metadata_and_handle_string_result_codes() {
    let bytes = Legacy::body(Dictionary::new()).unwrap();
    let body: Dictionary = plist::from_bytes(&bytes).unwrap();
    assert_eq!(body["clientId"].as_string(), Some("XABBG36SBA"));
    assert_eq!(body["protocolVersion"].as_string(), Some("QH65B2"));
    assert!(uuid::Uuid::parse_str(body["requestId"].as_string().unwrap()).is_ok());
    assert!(
        Legacy::url("ios/listApplicationGroups")
            .unwrap()
            .contains("QH65B2/ios/listApplicationGroups.action")
    );
    assert!(Legacy::url("../listTeams").is_err());
    let response = dictionary([("resultCode".into(), "0".into())]);
    assert!(Legacy::decode::<Dictionary>(&xml(&response)).is_ok());
    let error = dictionary([
        ("resultCode".into(), "42".into()),
        ("userString".into(), "Rejected".into()),
    ]);
    assert!(
        matches!(Legacy::decode::<Dictionary>(&xml(&error)),Err(Error::DeveloperApi { code, message }) if code == "42" && message == "Rejected")
    );
}

#[tokio::test]
async fn xcode_get_delete_and_patch_requests_transform_without_losing_query_parameters() {
    let client = client(MockHTTP::default().into());
    for method in [Method::GET, Method::DELETE] {
        let request = client
            .prepare_request(
                method.clone(),
                "v1/devices",
                &[
                    ("filter[name]".into(), "A & B".into()),
                    ("cursor".into(), "abc+def".into()),
                    ("limit".into(), "50".into()),
                    ("teamId".into(), "WRONG".into()),
                ],
                None,
            )
            .await
            .unwrap();
        assert_eq!(request.method, Method::POST);
        assert_eq!(request.headers["X-HTTP-Method-Override"], method.as_str());
        assert_eq!(request.headers["X-Apple-GS-Token"], "secret-token");
        assert_eq!(
            request.headers["X-Xcode-Version"],
            xkit::grand_slam::XCODE_VERSION
        );
        let body: Json = serde_json::from_slice(&request.body).unwrap();
        let url = reqwest::Url::parse(&format!(
            "https://example.test/?{}",
            body["urlEncodedQueryParams"].as_str().unwrap()
        ))
        .unwrap();
        let pairs = url
            .query_pairs()
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(pairs["teamId"], "TEAM123456");
        assert_eq!(pairs["filter[name]"], "A & B");
        assert_eq!(pairs["cursor"], "abc+def");
        assert!(reqwest::Url::parse(&request.url).unwrap().query().is_none());
    }
    let request = client.prepare_request(Method::PATCH,"v1/bundleIds/abc",&[],Some(json!({"data":{
        "id":"abc", "attributes":{"name":"Test","teamId":"WRONG"}, "relationships":{"foo":"bar"}}}))).await.unwrap();
    let body: Json = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body["data"]["attributes"]["teamId"], "TEAM123456");
    assert_eq!(body["data"]["relationships"]["foo"], "bar");
    assert!(
        client
            .prepare_request(Method::POST, "v1/devices", &[], Some(json!({"data":[]})))
            .await
            .is_err()
    );
    assert!(
        client
            .prepare_request(Method::GET, "https://example.com/v1/devices", &[], None)
            .await
            .is_err()
    );
}

#[test]
fn asc_jwts_have_verifiable_es256_signatures_and_renew_before_expiry() {
    let signing_key = SigningKey::random(&mut rand::rngs::OsRng);
    let key = ASCKey {
        id: "KEY".into(),
        issuer_id: "ISSUER".into(),
        pem: signing_key
            .to_pkcs8_pem(Default::default())
            .unwrap()
            .to_string(),
    };
    assert!(!format!("{key:?}").contains("PRIVATE KEY"));
    let generator = ASCJWTGenerator::new(key);
    let now = chrono::DateTime::from_timestamp(1_800_000_000, 0).unwrap();
    let jwt = generator.generate_at(now).unwrap();
    let parts: Vec<_> = jwt.split('.').collect();
    let header: Json = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
    let payload: Json = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
    assert_eq!(header["alg"], "ES256");
    assert_eq!(header["kid"], "KEY");
    assert_eq!(payload["aud"], "appstoreconnect-v1");
    assert_eq!(payload["iss"], "ISSUER");
    assert_eq!(
        payload["exp"].as_i64().unwrap() - payload["iat"].as_i64().unwrap(),
        900
    );
    let signature = Signature::from_slice(&URL_SAFE_NO_PAD.decode(parts[2]).unwrap()).unwrap();
    signing_key
        .verifying_key()
        .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
        .unwrap();
    assert_eq!(
        generator
            .generate_at(now + chrono::Duration::seconds(779))
            .unwrap(),
        jwt
    );
    assert_ne!(
        generator
            .generate_at(now + chrono::Duration::seconds(780))
            .unwrap(),
        jwt
    );
}

#[tokio::test]
async fn asc_requests_keep_native_methods_and_bearer_authentication() {
    let key = SigningKey::random(&mut rand::rngs::OsRng);
    let client = DeveloperAPIClient::with_http(
        DeveloperAPIAuthData::AppStoreConnect(ASCKey {
            id: "KEY".into(),
            issuer_id: "ISSUER".into(),
            pem: key.to_pkcs8_pem(Default::default()).unwrap().to_string(),
        }),
        None,
        Arc::new(MockHTTP::default()),
    )
    .unwrap();
    let request = client
        .prepare_request(
            Method::DELETE,
            "v1/certificates/id",
            &[("cursor".into(), "1".into())],
            None,
        )
        .await
        .unwrap();
    assert_eq!(request.method, Method::DELETE);
    assert!(
        request
            .url
            .starts_with("https://api.appstoreconnect.apple.com/")
    );
    assert!(
        request.headers["Authorization"]
            .to_str()
            .unwrap()
            .starts_with("Bearer ")
    );
    assert!(!request.headers.contains_key("X-Apple-GS-Token"));
    assert!(request.body.is_empty());
}

#[tokio::test]
async fn pagination_carries_limit_and_cursor_and_rejects_cycles() {
    let http = MockHTTP::json(vec![
        json!({"data":[resource("one","devices",json!({}))],"links":{"next":"https://ignored.example/v1/devices?cursor=abc%2Bdef&limit=25"}}),
        json!({"data":[resource("two","devices",json!({}))],"links":{"next":null}}),
    ]);
    let all = client(http.clone())
        .collection("v1/devices", &[])
        .await
        .unwrap();
    assert_eq!(
        all.iter().map(|v| v.id.as_str()).collect::<Vec<_>>(),
        vec!["one", "two"]
    );
    {
        let requests = http.requests.lock().unwrap();
        assert!(
            requests[1]
                .url
                .starts_with("https://developerservices2.apple.com/")
        );
        let body: Json = serde_json::from_slice(&requests[1].body).unwrap();
        assert!(
            body["urlEncodedQueryParams"]
                .as_str()
                .unwrap()
                .contains("cursor=abc%2Bdef&limit=25")
        );
    }
    http.done();
    assert!(next_link("https://example.com/?cursor=x").is_err());
    let page = json!({"data":[],"links":{"next":"https://example.com/?cursor=x&limit=10"}});
    assert!(
        client(MockHTTP::json(vec![page.clone(), page]))
            .collection("v1/devices", &[])
            .await
            .is_err()
    );
}

#[tokio::test]
async fn structured_api_errors_and_no_content_are_handled() {
    let http = MockHTTP::json(vec![
        json!({"errors":[{"code":"DENIED","detail":"Missing permission"}]}),
    ]);
    assert!(
        matches!(client(http).collection("v1/devices",&[]).await, Err(Error::DeveloperApi { code,.. }) if code == "DENIED")
    );
    let http = Arc::new(MockHTTP {
        requests: Mutex::default(),
        responses: Mutex::new(VecDeque::from([
            HTTPResponse {
                status: 204,
                headers: HeaderMap::new(),
                body: vec![],
            },
            HTTPResponse {
                status: 503,
                headers: HeaderMap::new(),
                body: b"Unavailable".to_vec(),
            },
        ])),
    });
    let client = client(http.clone());
    client.delete("v1/profiles/one").await.unwrap();
    assert!(matches!(
        client.delete("v1/profiles/two").await,
        Err(Error::Http { status: 503, .. })
    ));
    http.done();
}

#[tokio::test]
async fn certificate_reuse_does_not_revoke_or_create() {
    let info = signing_info();
    let manager = KeyValueSigningInfoManager::default();
    manager.set_info("TEAM123456", Some(&info)).unwrap();
    let http = MockHTTP::json(vec![
        json!({"data":[resource("cert-id","certificates",json!({
        "serialNumber":"3ab","expirationDate":"2099-01-01T00:00:00Z"}))]}),
    ]);
    let reused = client(http.clone())
        .fetch_certificate(&manager, &|_| panic!("Unexpected revocation"))
        .await
        .unwrap();
    assert_eq!(reused.certificate.data(), info.certificate.data());
    assert_eq!(http.requests.lock().unwrap().len(), 1);
    http.done();
}

#[tokio::test]
async fn free_team_certificate_revocation_can_be_cancelled() {
    let mut teams = Dictionary::new();
    teams.insert("resultCode".into(), Value::Integer(0.into()));
    let team = dictionary([
        ("teamId".into(), "TEAM123456".into()),
        ("status".into(), "active".into()),
        ("name".into(), "Test".into()),
        ("memberships".into(), Value::Array(vec![])),
    ]);
    teams.insert("teams".into(), Value::Array(vec![Value::Dictionary(team)]));
    let http = MockHTTP::json(vec![
        json!({"data":[resource("dev","certificates",json!({"certificateType":"DEVELOPMENT"})),
        resource("distribution","certificates",json!({"certificateType":"DISTRIBUTION"}))]}),
    ]);
    http.responses.lock().unwrap().push_back(HTTPResponse {
        status: 200,
        headers: HeaderMap::new(),
        body: xml(&teams),
    });
    let result = client(http.clone())
        .fetch_certificate(&KeyValueSigningInfoManager::default(), &|certs| {
            assert_eq!(certs.len(), 1);
            assert_eq!(certs[0].id, "dev");
            Box::pin(async { false })
        })
        .await;
    assert!(matches!(result, Err(Error::Cancelled)));
    assert_eq!(http.requests.lock().unwrap().len(), 2);
    http.done();
}

#[tokio::test]
async fn profile_creation_matches_exact_bundle_and_numeric_serial_across_pages() {
    let info = signing_info();
    let profile = profile_bytes(&info.certificate, "com.example.app");
    let http = MockHTTP::json(vec![
        json!({"data":[resource("wrong","bundleIds",json!({"identifier":"com.example.app.extension"})),
            {"id":"app-id","type":"bundleIds","attributes":{"identifier":"com.example.app"},"relationships":{"profiles":{"data":[]}}}]}),
        json!({"data":[resource("wrong-cert","certificates",json!({"serialNumber":"FFFF"}))],"links":{"next":"https://example.com/?cursor=next&limit=1"}}),
        json!({"data":[resource("right-cert","certificates",json!({"serialNumber":"0003ab"}))]}),
        json!({"data":[resource("iphone","devices",json!({"deviceClass":"IPHONE"})),resource("watch","devices",json!({"deviceClass":"APPLE_WATCH"}))]}),
        json!({"data":resource("profile-id","profiles",json!({"profileContent":STANDARD.encode(profile)}))}),
    ]);
    let profile = client(http.clone())
        .fetch_profile("com.example.app", &info)
        .await
        .unwrap();
    assert_eq!(profile.digest().uuid, "PROFILE-UUID");
    let requests = http.requests.lock().unwrap();
    let body: Json = serde_json::from_slice(&requests.last().unwrap().body).unwrap();
    assert_eq!(
        body["data"]["relationships"]["bundleId"]["data"]["id"],
        "app-id"
    );
    assert_eq!(
        body["data"]["relationships"]["certificates"]["data"],
        json!([{"id":"right-cert","type":"certificates"}])
    );
    assert_eq!(
        body["data"]["relationships"]["devices"]["data"],
        json!([{"id":"iphone","type":"devices"}])
    );
    drop(requests);
    http.done();
}

#[tokio::test]
async fn capabilities_are_created_updated_and_removed_while_required_purchase_is_kept() {
    let http = MockHTTP::json(vec![
        json!({"data":[resource("app-id","bundleIds",json!({"identifier":"XTL-TEAM123456.com.example.app"}))]}),
        json!({"data":[resource("health","bundleIdCapabilities",json!({"capabilityType":"HEALTHKIT","settings":[{"old":true}]})),
            resource("push","bundleIdCapabilities",json!({"capabilityType":"PUSH_NOTIFICATIONS"})),
            resource("purchase","bundleIdCapabilities",json!({"capabilityType":"IN_APP_PURCHASE"}))]}),
        json!({}),
        json!({}),
        json!({"data":resource("home","bundleIdCapabilities",json!({"capabilityType":"HOMEKIT"}))}),
    ]);
    let entitlements = Entitlements::new(vec![
        Entitlement::HealthKit(true),
        Entitlement::HomeKit(true),
    ])
    .unwrap();
    let app = client(http.clone())
        .upsert_app("XTL-OLD.com.example.app", &entitlements)
        .await
        .unwrap();
    assert_eq!(app.id, "app-id");
    let requests = http.requests.lock().unwrap();
    assert_eq!(requests.len(), 5);
    assert_eq!(requests[2].method, Method::PATCH);
    assert!(requests[3].url.ends_with("/push"));
    assert_eq!(requests[3].headers["X-HTTP-Method-Override"], "DELETE");
    let created: Json = serde_json::from_slice(&requests[4].body).unwrap();
    assert_eq!(
        created["data"]["relationships"]["capability"]["data"]["id"],
        "HOMEKIT"
    );
    drop(requests);
    http.done();
}

#[test]
fn ipa_roundtrip_preserves_payload_structure_and_empty_directories() {
    let temp = tempfile::tempdir().unwrap();
    let payload = temp.path().join("Payload");
    std::fs::create_dir_all(payload.join("Test.app/empty")).unwrap();
    std::fs::write(payload.join("Test.app/Test"), b"executable").unwrap();
    let archive = temp.path().join("test.ipa");
    ZIPCompressor::compress(&payload, &archive, &|_| {}).unwrap();
    let extracted = temp.path().join("extracted");
    ZIPCompressor::decompress(&archive, &extracted, &|_| {}).unwrap();
    assert_eq!(
        std::fs::read(extracted.join("Payload/Test.app/Test")).unwrap(),
        b"executable"
    );
    assert!(extracted.join("Payload/Test.app/empty").is_dir());
    assert!(ZIPCompressor::compress(&payload, &payload.join("bad.ipa"), &|_| {}).is_err());
}

#[tokio::test]
async fn legacy_team_and_platform_decoding_preserve_unknown_values() {
    let mut body = Dictionary::new();
    body.insert("resultCode".into(), 0.into());
    body.insert(
        "teams".into(),
        Value::Array(vec![Value::Dictionary(dictionary([
            ("teamId".into(), "TEAM".into()),
            ("status".into(), "active".into()),
            ("name".into(), "Test".into()),
            (
                "memberships".into(),
                Value::Array(vec![Value::Dictionary(dictionary([
                    ("name".into(), "Apple Developer Program".into()),
                    ("platform".into(), "ios".into()),
                ]))]),
            ),
        ]))]),
    );
    let http = Arc::new(MockHTTP {
        requests: Mutex::default(),
        responses: Mutex::new(VecDeque::from([HTTPResponse {
            status: 200,
            headers: HeaderMap::new(),
            body: xml(&body),
        }])),
    });
    let client =
        DeveloperServicesClient::with_http(token(), Arc::new(HeaderMap::new()), http.clone());
    assert!(!client.list_teams().await.unwrap()[0].is_free());
    assert_eq!(
        DeveloperServicesPlatform::from("futureOS".to_string()),
        DeveloperServicesPlatform::Unknown("futureOS".into())
    );
    assert_eq!(DeveloperServicesPlatform::TvOS.os(), "tvos");
    assert_eq!(DeveloperServicesPlatform::TvOS.sub_platform(), Some("tvOS"));
    http.done();
}

#[test]
fn real_macho_signing_assigns_separate_entitlements_to_app_and_extension() {
    use xkit::signer::{Signer, SigningIdentity};
    let temp = tempfile::tempdir().unwrap();
    let app = temp.path().join("Test.app");
    let extension = app.join("PlugIns/Extension.appex");
    write_app(&app, "com.example.app");
    write_app(&extension, "com.example.app.extension");
    let main = Entitlements::new(vec![
        Entitlement::ApplicationIdentifier("TEAM.main".into()),
        Entitlement::GetTaskAllow(true),
    ])
    .unwrap();
    let nested = Entitlements::new(vec![
        Entitlement::ApplicationIdentifier("TEAM.extension".into()),
        Entitlement::GetTaskAllow(true),
    ])
    .unwrap();
    let mapping = std::collections::BTreeMap::from([
        (app.clone(), main.clone()),
        (extension.clone(), nested.clone()),
    ]);
    Signer::sign(&app, SigningIdentity::Adhoc, &mapping, &|_| {}).unwrap();
    assert_eq!(Signer::analyze(&app.join("Test")).unwrap(), main);
    assert_eq!(Signer::analyze(&extension.join("Test")).unwrap(), nested);
    assert!(app.join("_CodeSignature/CodeResources").is_file());
    assert!(extension.join("_CodeSignature/CodeResources").is_file());
}

#[test]
fn rsa_keypair_generates_a_valid_csr_and_pem_private_key() {
    use rsa::pkcs8::DecodePrivateKey;
    let keypair = xkit::Keypair::new().unwrap();
    let private = keypair.private_key().unwrap();
    rsa::RsaPrivateKey::from_pkcs8_pem(std::str::from_utf8(private.data.as_ref()).unwrap())
        .unwrap();
    let csr = keypair.generate_csr().unwrap();
    assert!(csr.pem_string.contains("BEGIN CERTIFICATE REQUEST"));
    assert!(!format!("{private:?}").contains("BEGIN PRIVATE KEY"));
}

#[tokio::test]
async fn autosigner_provisions_embeds_and_signs_a_real_app_offline() {
    let raw = rcgen::Certificate::from_params({
        let mut params = rcgen::CertificateParams::new(Vec::new());
        params.serial_number = Some(rcgen::SerialNumber::from_slice(&[3, 0xab]));
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "Apple Development: Test");
        params
            .distinguished_name
            .push(rcgen::DnType::OrganizationalUnitName, "TEAM123456");
        params
    })
    .unwrap();
    let info = SigningInfo {
        certificate: Certificate::from_der(raw.serialize_der().unwrap()).unwrap(),
        private_key: xkit::PrivateKey {
            data: raw.serialize_private_key_pem().into_bytes().into(),
        },
    };
    let manager = Arc::new(KeyValueSigningInfoManager::default());
    manager.set_info("TEAM123456", Some(&info)).unwrap();
    let profile = profile_bytes(&info.certificate, "XTL-TEAM123456.com.example.app");
    let app_resource = resource(
        "app-id",
        "bundleIds",
        json!({"identifier":"XTL-TEAM123456.com.example.app"}),
    );
    let cert_resource = resource(
        "cert-id",
        "certificates",
        json!({"serialNumber":"3ab","expirationDate":"2099-01-01T00:00:00Z"}),
    );
    let http = MockHTTP::json(vec![json!({"data":[cert_resource.clone()]})]);
    let teams = dictionary([
        ("resultCode".into(), 0.into()),
        (
            "teams".into(),
            Value::Array(vec![Value::Dictionary(dictionary([
                ("teamId".into(), "TEAM123456".into()),
                ("status".into(), "active".into()),
                ("name".into(), "Free Team".into()),
                ("memberships".into(), Value::Array(vec![])),
            ]))]),
        ),
    ]);
    http.responses.lock().unwrap().push_back(HTTPResponse {
        status: 200,
        headers: HeaderMap::new(),
        body: xml(&teams),
    });
    for response in [
        json!({"data":[app_resource.clone()]}),
        json!({"data":[]}),
        json!({"data":[app_resource]}),
        json!({"data":[cert_resource]}),
        json!({"data":[resource("iphone","devices",json!({"deviceClass":"IPHONE"}))]}),
        json!({"data":resource("profile-id","profiles",json!({"profileContent":STANDARD.encode(&profile)}))}),
    ] {
        http.responses.lock().unwrap().push_back(HTTPResponse {
            status: 200,
            headers: HeaderMap::new(),
            body: serde_json::to_vec(&response).unwrap(),
        });
    }
    let temp = tempfile::tempdir().unwrap();
    let app = temp.path().join("Test.app");
    write_app(&app, "com.example.app");
    let signer = xkit::AutoSigner {
        context: xkit::SigningContext::new(client(http.clone()), manager),
    };
    let stages = Mutex::new(Vec::new());
    let result = signer
        .sign(
            &app,
            &|_| panic!("Certificate should be reused"),
            &|s| stages.lock().unwrap().push(s.to_string()),
            &|_| {},
            &|| Ok(()),
        )
        .await
        .unwrap();
    assert_eq!(result, "XTL-TEAM123456.com.example.app");
    let plist: Dictionary = plist::from_file(app.join("Info.plist")).unwrap();
    assert_eq!(
        plist["CFBundleIdentifier"].as_string(),
        Some(result.as_str())
    );
    assert_eq!(
        std::fs::read(app.join("embedded.mobileprovision")).unwrap(),
        profile
    );
    let entitlements = xkit::Signer::analyze(&app.join("Test")).unwrap();
    assert_eq!(
        entitlements.0["application-identifier"].as_string(),
        Some("TEAM123456.XTL-TEAM123456.com.example.app")
    );
    assert_eq!(entitlements.0["get-task-allow"].as_boolean(), Some(true));
    assert_eq!(*stages.lock().unwrap(), vec!["Provisioning", "Signing"]);
    http.done();
}
