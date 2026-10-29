use plist::Dictionary;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

use crate::Error;
use crate::auth::Account;
use crate::auth::anisette_data::AnisetteData;

impl Account {
    /// Sends an authenticated plist GET, or a POST when a body is supplied.
    pub async fn send_request(
        &self,
        url: &str,
        body: Option<Dictionary>,
    ) -> Result<Dictionary, Error> {
        let dsid = self
            .spd
            .as_ref()
            .and_then(|spd| spd.get("adsid"))
            .and_then(plist::Value::as_string)
            .ok_or_else(|| Error::AuthenticationResponse("Missing account ID".into()))?;
        let token = self.get_app_token("com.apple.gs.xcode.auth").await?;
        let anisette = self.get_anisette().await?;
        let headers = request_headers(&anisette, dsid, &token.auth_token)?;
        let request = if let Some(body) = body {
            let mut buffer = Vec::new();
            plist::to_writer_xml(&mut buffer, &body)?;
            self.client.post(url).headers(headers).body(buffer)
        } else {
            self.client.get(url).headers(headers)
        };
        let response = request.send().await?.error_for_status()?;
        Ok(plist::from_bytes(&response.bytes().await?)?)
    }
}

fn request_headers(anisette: &AnisetteData, dsid: &str, token: &str) -> Result<HeaderMap, Error> {
    let mut headers = HeaderMap::new();
    for (name, value) in anisette
        .generate_headers(false, true, true)
        .into_iter()
        .chain([
            ("Content-Type".into(), "text/x-xml-plist".into()),
            ("Accept".into(), "text/x-xml-plist".into()),
            ("Accept-Language".into(), "en-us".into()),
            ("User-Agent".into(), anisette.user_agent().into()),
            ("Connection".into(), "close".into()),
            ("X-Apple-I-Identity-Id".into(), dsid.into()),
            ("X-Apple-GS-Token".into(), token.into()),
        ])
    {
        let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
            Error::AuthenticationResponse("Invalid authentication header name".into())
        })?;
        let value = HeaderValue::from_str(&value).map_err(|_| {
            Error::AuthenticationResponse("Invalid authentication header value".into())
        })?;
        headers.insert(name, value);
    }
    Ok(headers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::HashMap, time::SystemTime};

    #[test]
    fn authenticated_requests_preserve_provider_identity_and_token_case() {
        let anisette = AnisetteData {
            base_headers: HashMap::from([
                (
                    "X-Mme-Client-Info".into(),
                    "<MacTest> <macOS> <com.apple.akd/1.0>".into(),
                ),
                ("User-Agent".into(), "akd/1.0 Provider/Test".into()),
                ("X-Apple-I-MD".into(), "AbC+/Otp==".into()),
            ]),
            generated_at: SystemTime::now(),
            config: Default::default(),
        };
        let headers = request_headers(&anisette, "123", "CaseSensitiveToken").unwrap();
        assert_eq!(headers["User-Agent"], "akd/1.0 Provider/Test");
        assert_eq!(
            headers["X-Mme-Client-Info"],
            anisette.base_headers["X-Mme-Client-Info"]
        );
        assert_eq!(headers["X-Apple-I-MD"], "AbC+/Otp==");
        assert_eq!(headers["X-Apple-GS-Token"], "CaseSensitiveToken");
        assert_eq!(headers["X-Apple-I-Identity-Id"], "123");
        assert!(request_headers(&anisette, "123", "invalid\nvalue").is_err());
    }
}
