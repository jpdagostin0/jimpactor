//! GrandSlam authentication uses the shared Rust SRP, AES, app-token and
//! trusted-device/SMS implementations rather than maintaining a second copy.
use crate::{Result, developer_services::DeveloperServicesLoginToken, invalid};
use futures::future::BoxFuture;
pub use plume_core::auth::anisette_data::AnisetteData;
pub use plume_core::auth::{
    Account, LoginState, TwoFactorAction, TwoFactorMethod, TwoFactorRequest,
};
pub use plume_core::{AnisetteConfiguration, AnisetteProvider};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use std::sync::Arc;
use tokio::sync::Mutex;

pub const XCODE_APP: &str = "com.apple.gs.xcode.auth";
pub const XCODE_VERSION: &str = "26.5 (17F42)";

pub trait AnisetteDataProvider: Send + Sync {
    fn headers(&self) -> BoxFuture<'_, Result<HeaderMap>>;
}

pub struct SharedAnisetteProvider(pub Arc<Mutex<AnisetteData>>);
impl AnisetteDataProvider for SharedAnisetteProvider {
    fn headers(&self) -> BoxFuture<'_, Result<HeaderMap>> {
        Box::pin(async move {
            let mut data = self.0.lock().await;
            if data.needs_refresh() {
                *data = data.refresh().await?;
            }
            let mut headers = HeaderMap::new();
            for (key, value) in &data.base_headers {
                headers.insert(
                    HeaderName::from_bytes(key.as_bytes()).map_err(|e| invalid(e.to_string()))?,
                    HeaderValue::from_str(value).map_err(|e| invalid(e.to_string()))?,
                );
            }
            Ok(headers)
        })
    }
}

/// Useful when the caller already owns a fresh anisette snapshot.
impl AnisetteDataProvider for HeaderMap {
    fn headers(&self) -> BoxFuture<'_, Result<HeaderMap>> {
        Box::pin(async { Ok(self.clone()) })
    }
}

pub struct DeveloperServicesLoginManager;
impl DeveloperServicesLoginManager {
    pub async fn log_in(
        username: &str,
        password: &str,
        config: AnisetteConfiguration,
        two_factor: impl Fn(TwoFactorRequest) -> std::result::Result<TwoFactorAction, String>,
    ) -> Result<(DeveloperServicesLoginToken, Arc<dyn AnisetteDataProvider>)> {
        // GSA requires lowercase usernames for SRP.
        let account = Account::login(
            || Ok((username.to_lowercase(), password.to_owned())),
            two_factor,
            config,
        )
        .await?;
        Self::from_account(&account).await
    }

    pub async fn from_account(
        account: &Account,
    ) -> Result<(DeveloperServicesLoginToken, Arc<dyn AnisetteDataProvider>)> {
        let adsid = account
            .spd
            .as_ref()
            .and_then(|d| d.get("adsid"))
            .and_then(plist::Value::as_string)
            .ok_or_else(|| invalid("Account is not logged in"))?
            .to_owned();
        let token = account.get_app_token(XCODE_APP).await?;
        let expiry_ms = token
            .app_tokens
            .get(XCODE_APP)
            .and_then(plist::Value::as_dictionary)
            .and_then(|d| d.get("expiry"))
            .and_then(|v| {
                v.as_signed_integer()
                    .or_else(|| v.as_string().and_then(|s| s.parse().ok()))
            })
            .ok_or_else(|| invalid("Missing Xcode app-token expiry"))?;
        let expiry = chrono::DateTime::from_timestamp_millis(expiry_ms)
            .ok_or_else(|| invalid("Invalid app-token expiry"))?;
        Ok((
            DeveloperServicesLoginToken {
                adsid,
                token: token.auth_token,
                expiry,
            },
            Arc::new(SharedAnisetteProvider(account.anisette.clone())),
        ))
    }
}
