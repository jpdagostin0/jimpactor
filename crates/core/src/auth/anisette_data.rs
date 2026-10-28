use std::collections::HashMap;
use std::time::SystemTime;

use omnisette::{AnisetteConfiguration, AnisetteHeaders};
use tokio::sync::Mutex;

use crate::Error;

#[derive(Debug, Clone)]
pub struct AnisetteData {
    pub base_headers: HashMap<String, String>,
    pub generated_at: SystemTime,
    pub config: AnisetteConfiguration,
}

impl AnisetteData {
    pub async fn new(config: AnisetteConfiguration) -> Result<Self, Error> {
        // The ADI libraries keep global state (including the provisioning path),
        // so only one local/fake provider may use them at a time.
        static ADI_LOCK: Mutex<()> = Mutex::const_new(());
        let _adi_guard = if config.provider().needs_libraries() {
            let guard = ADI_LOCK.lock().await;
            omnisette::adi_libraries::ensure_libraries(config.configuration_path()).await?;
            Some(guard)
        } else {
            None
        };

        let mut b = AnisetteHeaders::get_anisette_headers_provider(config.clone())?;
        let base_headers = b.provider.get_authentication_headers().await?;

        Ok(AnisetteData {
            base_headers,
            generated_at: SystemTime::now(),
            config,
        })
    }

    pub fn needs_refresh(&self) -> bool {
        let elapsed = self.generated_at.elapsed().unwrap_or_default();
        elapsed.as_secs() > 60
    }

    pub fn is_valid(&self) -> bool {
        let elapsed = self.generated_at.elapsed().unwrap_or_default();
        elapsed.as_secs() < 90
    }

    pub async fn refresh(&self) -> Result<Self, crate::Error> {
        Self::new(self.config.clone()).await
    }

    pub fn generate_headers(
        &self,
        cpd: bool,
        client_info: bool,
        app_info: bool,
    ) -> HashMap<String, String> {
        if !self.is_valid() {
            panic!("Invalid data!")
        }

        let mut headers = self.base_headers.clone();
        if !client_info {
            headers.retain(|key, _| !key.eq_ignore_ascii_case("x-mme-client-info"));
        }
        headers.retain(|key, _| !key.eq_ignore_ascii_case("user-agent"));

        if app_info {
            headers.insert(
                "X-Apple-App-Info".to_owned(),
                "com.apple.gs.xcode.auth".to_owned(),
            );
            headers.insert("X-Xcode-Version".to_owned(), "11.2 (11B41)".to_owned());
        }

        if cpd {
            headers.insert("bootstrap".to_owned(), "true".to_owned());
            headers.insert("icscrec".to_owned(), "true".to_owned());
            headers.insert("loc".to_owned(), "en_GB".to_owned());
            headers.insert("pbe".to_owned(), "false".to_owned());
            headers.insert("prkgen".to_owned(), "true".to_owned());
            headers.insert("svct".to_owned(), "iCloud".to_owned());
        }

        headers
    }

    pub fn to_plist(&self, cpd: bool, client_info: bool, app_info: bool) -> plist::Dictionary {
        let mut plist = plist::Dictionary::new();
        for (key, value) in self.generate_headers(cpd, client_info, app_info).iter() {
            plist.insert(key.to_owned(), plist::Value::String(value.to_owned()));
        }

        plist
    }

    pub fn user_agent(&self) -> &str {
        self.base_headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("user-agent"))
            .map(|(_, value)| value.as_str())
            .unwrap_or("akd/1.0 CFNetwork/978.0.7 Darwin/18.7.0")
    }

    pub fn get_header(&self, header: &str) -> Result<String, Error> {
        let headers = self
            .generate_headers(true, true, true)
            .iter()
            .map(|(k, v)| (k.to_lowercase(), v.clone()))
            .collect::<HashMap<String, String>>();

        match headers.get(&header.to_lowercase()) {
            Some(v) => Ok(v.to_string()),
            None => Err(Error::DeveloperSessionRequestFailed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_lookup_preserves_identity_values_and_accepts_short_client_info() {
        let data = AnisetteData {
            base_headers: HashMap::from([
                (
                    "X-Mme-Client-Info".into(),
                    "<MacBookPro> <macOS;14.0>".into(),
                ),
                ("X-Apple-I-MD".into(), "AbC+/Identity==".into()),
            ]),
            generated_at: SystemTime::now(),
            config: AnisetteConfiguration::new(),
        };
        assert_eq!(data.get_header("X-APPLE-I-MD").unwrap(), "AbC+/Identity==");
        assert_eq!(
            data.get_header("x-mme-client-info").unwrap(),
            "<MacBookPro> <macOS;14.0>"
        );
    }
}
