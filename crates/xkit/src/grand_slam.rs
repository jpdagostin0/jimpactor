//! GrandSlam constants and the anisette header abstraction used by the developer APIs.
//!
//! Logging in (SRP, 2FA, app tokens) and producing anisette data is left to the
//! caller: implement [`AnisetteDataProvider`] over whatever anisette source you use.
use crate::Result;
use futures::future::BoxFuture;
use reqwest::header::HeaderMap;

pub const XCODE_APP: &str = "com.apple.gs.xcode.auth";
pub const XCODE_VERSION: &str = "26.5 (17F42)";

pub trait AnisetteDataProvider: Send + Sync {
    fn headers(&self) -> BoxFuture<'_, Result<HeaderMap>>;
}

/// Useful when the caller already owns a fresh anisette snapshot.
impl AnisetteDataProvider for HeaderMap {
    fn headers(&self) -> BoxFuture<'_, Result<HeaderMap>> {
        Box::pin(async { Ok(self.clone()) })
    }
}
