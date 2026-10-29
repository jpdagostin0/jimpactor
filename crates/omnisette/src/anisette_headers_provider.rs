use std::collections::HashMap;

use crate::AnisetteError;

/// Older anisette servers advertise Xcode, which Apple's GrandSlam edge rejects.
/// Keep the server's hardware/OS identity and only update the AuthKit client.
pub(crate) fn normalize_client_info(client_info: &str) -> String {
    let mut client_info = client_info.to_owned();
    if let Some(start) = client_info.find("com.apple.dt.Xcode") {
        let end = client_info[start..]
            .find(|c: char| c == ')' || c == '>' || c.is_whitespace())
            .map_or(client_info.len(), |end| start + end);
        client_info.replace_range(start..end, "com.apple.akd/1.0");
    }
    client_info
}

#[cfg_attr(feature = "async", async_trait::async_trait)]
pub trait AnisetteHeadersProvider: Send + Sync {
    #[cfg_attr(not(feature = "async"), remove_async_await::remove_async_await)]
    async fn get_anisette_headers(
        &mut self,
        skip_provisioning: bool,
    ) -> Result<HashMap<String, String>, AnisetteError>;

    #[cfg_attr(not(feature = "async"), remove_async_await::remove_async_await)]
    async fn get_authentication_headers(
        &mut self,
    ) -> Result<HashMap<String, String>, AnisetteError> {
        let headers = self.get_anisette_headers(false).await?;
        Ok(self.normalize_headers(headers))
    }

    /// Normalizes headers to ensure that all the required headers are given.
    fn normalize_headers(
        &mut self,
        mut headers: HashMap<String, String>,
    ) -> HashMap<String, String> {
        if let Some(client_info) = headers.remove("X-MMe-Client-Info") {
            headers.insert("X-Mme-Client-Info".to_string(), client_info);
        }
        for (name, value) in &mut headers {
            if name.eq_ignore_ascii_case("X-Mme-Client-Info") {
                *value = normalize_client_info(value);
            }
        }

        headers
    }
}
