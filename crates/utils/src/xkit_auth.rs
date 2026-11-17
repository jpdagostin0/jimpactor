//! Adapts `plume_core`'s GrandSlam account and anisette to `xkit`'s provider traits.
use std::sync::Arc;

use futures::future::BoxFuture;
use plume_core::{
    AnisetteConfiguration,
    auth::{Account, TwoFactorAction, TwoFactorRequest, anisette_data::AnisetteData},
};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use tokio::sync::Mutex;
use xkit::{
    DeveloperServicesLoginToken,
    grand_slam::{AnisetteDataProvider, XCODE_APP},
};

fn invalid(message: impl Into<String>) -> xkit::Error {
    xkit::Error::InvalidData(message.into())
}

fn external(error: plume_core::Error) -> xkit::Error {
    xkit::Error::External(Box::new(error))
}

pub struct SharedAnisetteProvider(pub Arc<Mutex<AnisetteData>>);
impl AnisetteDataProvider for SharedAnisetteProvider {
    fn headers(&self) -> BoxFuture<'_, xkit::Result<HeaderMap>> {
        Box::pin(async move {
            let mut data = self.0.lock().await;
            if data.needs_refresh() {
                *data = data.refresh().await.map_err(external)?;
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

pub struct DeveloperServicesLoginManager;
impl DeveloperServicesLoginManager {
    pub async fn log_in(
        username: &str,
        password: &str,
        config: AnisetteConfiguration,
        two_factor: impl Fn(TwoFactorRequest) -> std::result::Result<TwoFactorAction, String>,
    ) -> xkit::Result<(DeveloperServicesLoginToken, Arc<dyn AnisetteDataProvider>)> {
        // GSA requires lowercase usernames for SRP.
        let account = Account::login(
            || Ok((username.to_lowercase(), password.to_owned())),
            two_factor,
            config,
        )
        .await
        .map_err(external)?;
        Self::from_account(&account).await
    }

    pub async fn from_account(
        account: &Account,
    ) -> xkit::Result<(DeveloperServicesLoginToken, Arc<dyn AnisetteDataProvider>)> {
        let adsid = account
            .spd
            .as_ref()
            .and_then(|d| d.get("adsid"))
            .and_then(plist::Value::as_string)
            .ok_or_else(|| invalid("Account is not logged in"))?
            .to_owned();
        let token = account.get_app_token(XCODE_APP).await.map_err(external)?;
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
