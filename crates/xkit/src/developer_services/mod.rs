//! Legacy plist and JSON:API developer services, with Xcode and ASC authentication.
mod operations;
pub use operations::*;

use crate::{
    Error, Result,
    grand_slam::{AnisetteDataProvider, XCODE_APP, XCODE_VERSION},
    http::{HTTPClient, HTTPClientProtocol, HTTPRequest},
    invalid,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use p256::{
    ecdsa::{Signature, SigningKey, signature::Signer as _},
    pkcs8::DecodePrivateKey,
};
use plist::{Dictionary, Value};
use reqwest::{
    Method, Url,
    header::{HeaderMap, HeaderName, HeaderValue},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value as Json, json};
use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};

const DS_BASE: &str = "https://developerservices2.apple.com/services/";
const ASC_BASE: &str = "https://api.appstoreconnect.apple.com/";
const CLIENT_ID: &str = "XABBG36SBA";
const PROTOCOL_VERSION: &str = "QH65B2";

#[derive(Clone, Serialize, Deserialize)]
pub struct DeveloperServicesLoginToken {
    pub adsid: String,
    pub token: String,
    pub expiry: DateTime<Utc>,
}
impl std::fmt::Debug for DeveloperServicesLoginToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeveloperServicesLoginToken")
            .field("expiry", &self.expiry)
            .finish_non_exhaustive()
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct XcodeAuthData {
    pub login_token: DeveloperServicesLoginToken,
    pub team_id: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct ASCKey {
    pub id: String,
    pub issuer_id: String,
    pub pem: String,
}
impl std::fmt::Debug for ASCKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ASCKey")
            .field("id", &self.id)
            .field("issuer_id", &self.issuer_id)
            .finish_non_exhaustive()
    }
}
#[derive(Clone, Debug)]
pub enum DeveloperAPIAuthData {
    AppStoreConnect(ASCKey),
    Xcode(XcodeAuthData),
}
impl DeveloperAPIAuthData {
    pub fn identity_id(&self) -> &str {
        match self {
            Self::AppStoreConnect(key) => &key.issuer_id,
            Self::Xcode(auth) => &auth.team_id,
        }
    }
    pub fn is_public_api(&self) -> bool {
        matches!(self, Self::AppStoreConnect(_))
    }
}

pub struct ASCJWTGenerator {
    key: ASCKey,
    cached: Mutex<Option<(String, DateTime<Utc>)>>,
}
impl ASCJWTGenerator {
    pub fn new(key: ASCKey) -> Self {
        Self {
            key,
            cached: Mutex::new(None),
        }
    }
    pub fn generate(&self) -> Result<String> {
        self.generate_at(Utc::now())
    }
    pub fn generate_at(&self, now: DateTime<Utc>) -> Result<String> {
        let mut cached = self
            .cached
            .lock()
            .map_err(|_| invalid("JWT cache lock poisoned"))?;
        if let Some((jwt, renew_at)) = cached.as_ref()
            && *renew_at > now
        {
            return Ok(jwt.clone());
        }
        let key = SigningKey::from_pkcs8_pem(&self.key.pem).map_err(|e| invalid(e.to_string()))?;
        let header = URL_SAFE_NO_PAD.encode(serde_json::to_vec(
            &json!({"alg":"ES256", "typ":"JWT", "kid":self.key.id}),
        )?);
        let expiry = now + chrono::Duration::minutes(15);
        let payload = URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&json!({"aud":"appstoreconnect-v1",
            "iss":self.key.issuer_id, "iat":now.timestamp(), "exp":expiry.timestamp()}))?);
        let body = format!("{header}.{payload}");
        let signature: Signature = key.sign(body.as_bytes());
        let jwt = format!("{body}.{}", URL_SAFE_NO_PAD.encode(signature.to_bytes()));
        *cached = Some((jwt.clone(), expiry - chrono::Duration::minutes(2)));
        Ok(jwt)
    }
}

fn header(headers: &mut HeaderMap, name: &str, value: &str) -> Result<()> {
    headers.insert(
        HeaderName::from_bytes(name.as_bytes()).map_err(|e| invalid(e.to_string()))?,
        HeaderValue::from_str(value).map_err(|e| invalid(e.to_string()))?,
    );
    Ok(())
}

async fn xcode_headers(
    token: &DeveloperServicesLoginToken,
    anisette: &dyn AnisetteDataProvider,
) -> Result<HeaderMap> {
    let mut headers = anisette.headers().await?;
    for (name, value) in [
        ("Accept-Language", "en-US"),
        ("User-Agent", "Xcode"),
        ("X-Xcode-Version", XCODE_VERSION),
        ("X-Apple-App-Info", XCODE_APP),
        ("X-Apple-I-Identity-Id", token.adsid.as_str()),
        ("X-Apple-GS-Token", token.token.as_str()),
    ] {
        header(&mut headers, name, value)?;
    }
    Ok(headers)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum DeveloperServicesPlatform {
    MacOS,
    IOS,
    WatchOS,
    TvOS,
    Safari,
    Unknown(String),
}
impl From<String> for DeveloperServicesPlatform {
    fn from(value: String) -> Self {
        match value.as_str() {
            "mac" => Self::MacOS,
            "ios" => Self::IOS,
            "watchOS" => Self::WatchOS,
            "tvOS" => Self::TvOS,
            "safari" => Self::Safari,
            _ => Self::Unknown(value),
        }
    }
}
impl From<DeveloperServicesPlatform> for String {
    fn from(value: DeveloperServicesPlatform) -> Self {
        value.raw_value().to_owned()
    }
}
impl DeveloperServicesPlatform {
    pub fn raw_value(&self) -> &str {
        match self {
            Self::MacOS => "mac",
            Self::IOS => "ios",
            Self::WatchOS => "watchOS",
            Self::TvOS => "tvOS",
            Self::Safari => "safari",
            Self::Unknown(value) => value,
        }
    }
    pub fn os(&self) -> &str {
        match self {
            Self::IOS | Self::WatchOS => "ios",
            Self::TvOS => "tvos",
            _ => "",
        }
    }
    pub fn sub_platform(&self) -> Option<&str> {
        match self {
            Self::IOS | Self::WatchOS => None,
            Self::TvOS => Some("tvOS"),
            _ => Some(""),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct DeveloperServicesTeam {
    #[serde(rename = "teamId")]
    pub id: String,
    pub status: String,
    pub name: String,
    pub memberships: Vec<TeamMembership>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct TeamMembership {
    pub name: String,
    pub platform: DeveloperServicesPlatform,
}
impl DeveloperServicesTeam {
    pub fn is_free(&self) -> bool {
        !self.memberships.iter().any(|m| {
            m.platform == DeveloperServicesPlatform::IOS
                && m.name.contains("Apple Developer Program")
        })
    }
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Developer {
    pub email: String,
    pub first_name: String,
    pub last_name: String,
    pub ds_first_name: String,
    pub ds_last_name: String,
    pub developer_status: String,
}
#[derive(Clone, Debug, Deserialize)]
pub struct DeveloperServicesAppGroup {
    #[serde(rename = "applicationGroup")]
    pub id: String,
    pub name: String,
    #[serde(rename = "identifier")]
    pub group_id: String,
}

pub struct DeveloperServicesAPIVersionLegacy;
impl DeveloperServicesAPIVersionLegacy {
    pub fn url(action: &str) -> Result<String> {
        if action
            .split('/')
            .any(|v| v.is_empty() || !v.chars().all(|c| c.is_ascii_alphanumeric()))
        {
            return Err(invalid("Malformed developer-services action"));
        }
        Ok(format!(
            "{DS_BASE}{PROTOCOL_VERSION}/{action}.action?clientId={CLIENT_ID}"
        ))
    }
    pub fn body(mut parameters: Dictionary) -> Result<Vec<u8>> {
        parameters.insert(
            "requestId".into(),
            uuid::Uuid::new_v4().to_string().to_uppercase().into(),
        );
        parameters.insert("clientId".into(), CLIENT_ID.into());
        parameters.insert("protocolVersion".into(), PROTOCOL_VERSION.into());
        parameters.insert("userLocale".into(), Value::Array(vec!["en_US".into()]));
        let mut bytes = Vec::new();
        plist::to_writer_xml(&mut bytes, &parameters)?;
        Ok(bytes)
    }
    pub fn decode<T: DeserializeOwned>(response: &[u8]) -> Result<T> {
        let value: Value = plist::from_bytes(response)?;
        let dict = value
            .as_dictionary()
            .ok_or_else(|| invalid("Expected developer response dictionary"))?;
        let code = dict
            .get("resultCode")
            .and_then(|v| {
                v.as_signed_integer()
                    .or_else(|| v.as_unsigned_integer().and_then(|v| v.try_into().ok()))
                    .or_else(|| v.as_string().and_then(|v| v.parse().ok()))
            })
            .ok_or_else(|| invalid("Missing developer result code"))?;
        if code != 0 {
            let message = dict
                .get("userString")
                .or_else(|| dict.get("resultString"))
                .and_then(Value::as_string)
                .unwrap_or("Unknown developer API error")
                .into();
            return Err(Error::DeveloperApi {
                code: code.to_string(),
                message,
            });
        }
        Ok(plist::from_value(&value)?)
    }
}

#[derive(Clone)]
pub struct DeveloperServicesClient {
    pub login_token: DeveloperServicesLoginToken,
    http: Arc<dyn HTTPClientProtocol>,
    anisette: Arc<dyn AnisetteDataProvider>,
}
impl DeveloperServicesClient {
    pub fn new(
        login_token: DeveloperServicesLoginToken,
        anisette: Arc<dyn AnisetteDataProvider>,
    ) -> Result<Self> {
        Ok(Self::with_http(
            login_token,
            anisette,
            Arc::new(HTTPClient::new()?),
        ))
    }
    pub fn with_http(
        login_token: DeveloperServicesLoginToken,
        anisette: Arc<dyn AnisetteDataProvider>,
        http: Arc<dyn HTTPClientProtocol>,
    ) -> Self {
        Self {
            login_token,
            http,
            anisette,
        }
    }
    pub async fn send<T: DeserializeOwned>(
        &self,
        action: &str,
        parameters: Dictionary,
    ) -> Result<T> {
        let mut request = HTTPRequest::new(
            Method::POST,
            DeveloperServicesAPIVersionLegacy::url(action)?,
        );
        request.headers = xcode_headers(&self.login_token, self.anisette.as_ref()).await?;
        header(&mut request.headers, "Accept", "text/x-xml-plist")?;
        header(&mut request.headers, "Content-Type", "text/x-xml-plist")?;
        request.body = DeveloperServicesAPIVersionLegacy::body(parameters)?;
        let response = self
            .http
            .send(request.clone())
            .await?
            .require_success(&request)?;
        DeveloperServicesAPIVersionLegacy::decode(&response.body)
    }
    pub async fn list_teams(&self) -> Result<Vec<DeveloperServicesTeam>> {
        #[derive(Deserialize)]
        struct Response {
            teams: Vec<DeveloperServicesTeam>,
        }
        Ok(self
            .send::<Response>("listTeams", Dictionary::new())
            .await?
            .teams)
    }
    pub async fn view_developer(&self) -> Result<Developer> {
        #[derive(Deserialize)]
        struct Response {
            developer: Developer,
        }
        Ok(self
            .send::<Response>("viewDeveloper", Dictionary::new())
            .await?
            .developer)
    }
    fn platform_parameters(platform: &DeveloperServicesPlatform, team_id: &str) -> Dictionary {
        let mut parameters = Dictionary::new();
        parameters.insert("teamId".into(), team_id.into());
        parameters.insert("DTDK_Platform".into(), platform.raw_value().into());
        if let Some(sub) = platform.sub_platform() {
            parameters.insert("subPlatform".into(), sub.into());
        }
        parameters
    }
    pub async fn list_app_groups(
        &self,
        platform: &DeveloperServicesPlatform,
        team_id: &str,
    ) -> Result<Vec<DeveloperServicesAppGroup>> {
        #[derive(Deserialize)]
        struct Response {
            #[serde(rename = "applicationGroupList")]
            groups: Vec<DeveloperServicesAppGroup>,
        }
        Ok(self
            .send::<Response>(
                &format!("{}/listApplicationGroups", platform.os()),
                Self::platform_parameters(platform, team_id),
            )
            .await?
            .groups)
    }
    pub async fn add_app_group(
        &self,
        platform: &DeveloperServicesPlatform,
        team_id: &str,
        name: &str,
        group_id: &str,
    ) -> Result<DeveloperServicesAppGroup> {
        #[derive(Deserialize)]
        struct Response {
            #[serde(rename = "applicationGroup")]
            group: DeveloperServicesAppGroup,
        }
        let mut parameters = Self::platform_parameters(platform, team_id);
        parameters.insert("name".into(), name.into());
        parameters.insert("identifier".into(), group_id.into());
        Ok(self
            .send::<Response>(
                &format!("{}/addApplicationGroup", platform.os()),
                parameters,
            )
            .await?
            .group)
    }
    pub async fn assign_app_group(
        &self,
        platform: &DeveloperServicesPlatform,
        team_id: &str,
        app_id: &str,
        group_id: &str,
    ) -> Result<()> {
        let mut parameters = Self::platform_parameters(platform, team_id);
        parameters.insert("appIdId".into(), app_id.into());
        parameters.insert("applicationGroups".into(), group_id.into());
        self.send::<Value>(
            &format!("{}/assignApplicationGroupToAppId", platform.os()),
            parameters,
        )
        .await?;
        Ok(())
    }
}

/// JSON:API resources retain unknown attributes, relationship types and enum values.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Resource {
    pub id: String,
    #[serde(rename = "type")]
    pub resource_type: String,
    #[serde(default)]
    pub attributes: serde_json::Map<String, Json>,
    #[serde(default)]
    pub relationships: serde_json::Map<String, Json>,
}
impl Resource {
    pub fn attribute(&self, key: &str) -> Option<&str> {
        self.attributes.get(key)?.as_str()
    }
}
pub type DeveloperServicesCertificate = Resource;

/// How provisioned bundle and app group identifiers derive from the app's own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IdentifierPolicy {
    /// XTool's `XTL-<identity>.<id>` scheme.
    #[default]
    Xtool,
    /// Use the identifiers already in the bundle; app groups gain a `.<team>` suffix.
    Verbatim,
}

#[derive(Clone)]
pub struct DeveloperAPIClient {
    pub auth: DeveloperAPIAuthData,
    pub identifier_policy: IdentifierPolicy,
    http: Arc<dyn HTTPClientProtocol>,
    anisette: Option<Arc<dyn AnisetteDataProvider>>,
    jwt: Option<Arc<ASCJWTGenerator>>,
    team: Arc<tokio::sync::Mutex<Option<DeveloperServicesTeam>>>,
}
impl DeveloperAPIClient {
    pub fn new(
        auth: DeveloperAPIAuthData,
        anisette: Option<Arc<dyn AnisetteDataProvider>>,
    ) -> Result<Self> {
        Self::with_http(auth, anisette, Arc::new(HTTPClient::new()?))
    }
    pub fn with_http(
        auth: DeveloperAPIAuthData,
        anisette: Option<Arc<dyn AnisetteDataProvider>>,
        http: Arc<dyn HTTPClientProtocol>,
    ) -> Result<Self> {
        if matches!(auth, DeveloperAPIAuthData::Xcode(_)) && anisette.is_none() {
            return Err(invalid("Xcode authentication requires anisette"));
        }
        let jwt = match &auth {
            DeveloperAPIAuthData::AppStoreConnect(key) => {
                Some(Arc::new(ASCJWTGenerator::new(key.clone())))
            }
            _ => None,
        };
        Ok(Self {
            auth,
            identifier_policy: IdentifierPolicy::default(),
            http,
            anisette,
            jwt,
            team: Arc::new(tokio::sync::Mutex::new(None)),
        })
    }
    pub fn with_identifier_policy(mut self, policy: IdentifierPolicy) -> Self {
        self.identifier_policy = policy;
        self
    }
    pub fn legacy(&self) -> Result<DeveloperServicesClient> {
        match (&self.auth, &self.anisette) {
            (DeveloperAPIAuthData::Xcode(auth), Some(anisette)) => {
                Ok(DeveloperServicesClient::with_http(
                    auth.login_token.clone(),
                    anisette.clone(),
                    self.http.clone(),
                ))
            }
            _ => Err(Error::RequiresXcode),
        }
    }
    pub async fn team(&self) -> Result<Option<DeveloperServicesTeam>> {
        let DeveloperAPIAuthData::Xcode(auth) = &self.auth else {
            return Ok(None);
        };
        let mut cached = self.team.lock().await;
        if let Some(team) = cached.as_ref() {
            return Ok(Some(team.clone()));
        }
        let team = self
            .legacy()?
            .list_teams()
            .await?
            .into_iter()
            .find(|t| t.id == auth.team_id)
            .ok_or_else(|| invalid(format!("Team '{}' was not found", auth.team_id)))?;
        *cached = Some(team.clone());
        Ok(Some(team))
    }

    pub async fn request(
        &self,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<Json>,
    ) -> Result<Json> {
        let request = self.prepare_request(method, path, query, body).await?;
        let response = self.http.send(request.clone()).await?;
        let decoded = if response.body.is_empty() {
            Json::Null
        } else {
            match serde_json::from_slice::<Json>(&response.body) {
                Ok(value) => value,
                Err(_) if !(200..300).contains(&response.status) => {
                    response.require_success(&request)?;
                    unreachable!()
                }
                Err(error) => return Err(error.into()),
            }
        };
        if let Some(errors) = decoded.get("errors").and_then(Json::as_array)
            && !errors.is_empty()
        {
            return Err(Error::DeveloperApi {
                code: errors[0]
                    .get("code")
                    .and_then(Json::as_str)
                    .unwrap_or("unknown")
                    .into(),
                message: errors
                    .iter()
                    .map(|e| {
                        e.get("detail")
                            .or_else(|| e.get("title"))
                            .and_then(Json::as_str)
                            .unwrap_or("Unknown developer API error")
                    })
                    .collect::<Vec<_>>()
                    .join("; "),
            });
        }
        response.require_success(&request)?;
        Ok(decoded)
    }

    pub async fn prepare_request(
        &self,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<Json>,
    ) -> Result<HTTPRequest> {
        // Only relative API paths are accepted: pagination links never select a host
        // to which credentials could accidentally be forwarded.
        if !path.starts_with("v1/") || path.contains("..") || path.contains(['?', '#', '\\']) {
            return Err(invalid("Expected a relative v1 developer API path"));
        }
        let base = if self.auth.is_public_api() {
            ASC_BASE
        } else {
            DS_BASE
        };
        let mut url = Url::parse(&format!("{base}{path}")).map_err(|e| invalid(e.to_string()))?;
        url.query_pairs_mut()
            .extend_pairs(query.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        let mut request = HTTPRequest::new(method.clone(), url.as_str());
        header(&mut request.headers, "Accept", "application/vnd.api+json")?;
        header(
            &mut request.headers,
            "Content-Type",
            "application/vnd.api+json",
        )?;
        match &self.auth {
            DeveloperAPIAuthData::AppStoreConnect(_) => {
                let jwt = self
                    .jwt
                    .as_ref()
                    .ok_or_else(|| invalid("Missing JWT generator"))?
                    .generate()?;
                header(
                    &mut request.headers,
                    "Authorization",
                    &format!("Bearer {jwt}"),
                )?;
                if let Some(body) = body {
                    request.body = serde_json::to_vec(&body)?;
                }
            }
            DeveloperAPIAuthData::Xcode(auth) => {
                let anisette = self
                    .anisette
                    .as_ref()
                    .ok_or_else(|| invalid("Missing anisette provider"))?;
                request
                    .headers
                    .extend(xcode_headers(&auth.login_token, anisette.as_ref()).await?);
                match method {
                    Method::GET | Method::DELETE => {
                        header(
                            &mut request.headers,
                            "X-HTTP-Method-Override",
                            method.as_str(),
                        )?;
                        request.method = Method::POST;
                        // Replace any user-provided teamId, as the auth identity owns it.
                        let mut pairs: Vec<_> = url
                            .query_pairs()
                            .filter(|(k, _)| k != "teamId")
                            .map(|(k, v)| (k.into_owned(), v.into_owned()))
                            .collect();
                        pairs.push(("teamId".into(), auth.team_id.clone()));
                        url.set_query(None);
                        url.query_pairs_mut().extend_pairs(&pairs);
                        request.body = serde_json::to_vec(
                            &json!({"urlEncodedQueryParams":url.query().unwrap_or("")}),
                        )?;
                        url.set_query(None);
                        request.url = url.into();
                    }
                    Method::POST | Method::PATCH => {
                        let mut body = body.unwrap_or_else(|| json!({}));
                        let object = body
                            .as_object_mut()
                            .ok_or_else(|| invalid("Malformed API body"))?;
                        let data = object
                            .entry("data")
                            .or_insert_with(|| json!({}))
                            .as_object_mut()
                            .ok_or_else(|| invalid("Malformed API data"))?;
                        let attributes = data
                            .entry("attributes")
                            .or_insert_with(|| json!({}))
                            .as_object_mut()
                            .ok_or_else(|| invalid("Malformed API attributes"))?;
                        attributes.insert("teamId".into(), auth.team_id.clone().into());
                        request.body = serde_json::to_vec(&body)?;
                    }
                    _ => {
                        return Err(invalid(format!(
                            "Unsupported developer API method {method}"
                        )));
                    }
                }
            }
        }
        Ok(request)
    }

    pub async fn collection(
        &self,
        path: &str,
        query: &[(String, String)],
    ) -> Result<Vec<Resource>> {
        let mut result = Vec::new();
        let mut query = query.to_vec();
        let mut seen = HashSet::new();
        loop {
            let page = self.request(Method::GET, path, &query, None).await?;
            let data = page
                .get("data")
                .ok_or_else(|| invalid("Missing JSON:API data"))?;
            result.extend(serde_json::from_value::<Vec<Resource>>(data.clone())?);
            let Some(next) = page.pointer("/links/next").and_then(Json::as_str) else {
                break;
            };
            if !seen.insert(next.to_owned()) {
                return Err(invalid("Developer API pagination cycle"));
            }
            let (cursor, limit) = next_link(next)?;
            query.retain(|(key, _)| key != "cursor" && key != "limit");
            query.push(("cursor".into(), cursor));
            query.push(("limit".into(), limit));
        }
        Ok(result)
    }
    pub async fn create(&self, path: &str, data: Json) -> Result<Resource> {
        let response = self
            .request(Method::POST, path, &[], Some(json!({"data":data})))
            .await?;
        Ok(serde_json::from_value(
            response
                .get("data")
                .cloned()
                .ok_or_else(|| invalid("Missing JSON:API resource"))?,
        )?)
    }
    pub async fn delete(&self, path: &str) -> Result<()> {
        self.request(Method::DELETE, path, &[], None).await?;
        Ok(())
    }
}

pub fn next_link(link: &str) -> Result<(String, String)> {
    let url = Url::parse(link)
        .or_else(|_| Url::parse(ASC_BASE).and_then(|base| base.join(link)))
        .map_err(|e| invalid(e.to_string()))?;
    let pairs: Vec<_> = url.query_pairs().collect();
    let get = |name: &str| {
        pairs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, v)| v.to_string())
    };
    match (get("cursor"), get("limit")) {
        (Some(cursor), Some(limit))
            if !cursor.is_empty() && limit.parse::<u32>().is_ok_and(|v| v > 0) =>
        {
            Ok((cursor, limit))
        }
        _ => Err(invalid(format!("Bad developer API next link: {link}"))),
    }
}
