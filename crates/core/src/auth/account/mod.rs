mod login;
mod request;
mod token;
mod two_factor_auth;

use aes::cipher::BlockModeDecrypt;
use cbc::cipher::{KeyIvInit, block_padding::Pkcs7};
use hmac::{Hmac, KeyInit, Mac};
use reqwest::Response;
use sha2::Sha256;
use srp::ClientVerifier;

use crate::Error;

pub async fn parse_response(
    res: Result<Response, reqwest::Error>,
) -> Result<plist::Dictionary, Error> {
    let res = res?;
    let status = res.status();

    if !status.is_success() {
        return Err(Error::GeneralHttpError {
            url: res.url().to_string(),
            status: status.as_u16(),
            // body: res.text().await?,
        });
    }

    let url = res.url().to_string();
    let bytes = res.bytes().await?;
    parse_auth_response(&bytes, &url)
}

fn parse_auth_response(bytes: &[u8], url: &str) -> Result<plist::Dictionary, Error> {
    let outer: plist::Dictionary = plist::from_bytes(bytes).map_err(|_| {
        Error::AuthenticationResponse(format!("Expected an Apple authentication plist from {url}. Check the anisette provider and any proxy or sign-in portal."))
    })?;
    outer
        .get("Response")
        .and_then(plist::Value::as_dictionary)
        .cloned()
        .ok_or_else(|| Error::AuthenticationResponse("Missing Response dictionary".into()))
}

pub fn check_error(res: &plist::Dictionary) -> Result<(), Error> {
    let status = match res.get("Status") {
        Some(plist::Value::Dictionary(d)) => d,
        Some(_) => {
            return Err(Error::AuthenticationResponse(
                "Invalid Status dictionary".into(),
            ));
        }
        None => res,
    };
    let code = status
        .get("ec")
        .and_then(plist::Value::as_signed_integer)
        .ok_or_else(|| {
            Error::AuthenticationResponse("Missing authentication status code".into())
        })?;
    if code != 0 {
        let message = status
            .get("em")
            .and_then(plist::Value::as_string)
            .unwrap_or("Apple rejected authentication");
        return Err(Error::AuthSrpWithMessage(code, message.to_owned()));
    }
    Ok(())
}

pub fn decrypt_cbc(usr: &ClientVerifier<Sha256>, data: &[u8]) -> Result<Vec<u8>, Error> {
    let extra_data_key = create_session_key(usr, "extra data key:");
    let extra_data_iv = create_session_key(usr, "extra data iv:");
    let extra_data_iv = &extra_data_iv[..16];

    cbc::Decryptor::<aes::Aes256>::new_from_slices(&extra_data_key, extra_data_iv)
        .unwrap()
        .decrypt_padded_vec::<Pkcs7>(data)
        .map_err(|_| Error::AuthenticationResponse("Invalid encrypted session data".into()))
}

pub fn create_session_key(usr: &ClientVerifier<Sha256>, name: &str) -> Vec<u8> {
    Hmac::<Sha256>::new_from_slice(usr.key())
        .unwrap()
        .chain_update(name.as_bytes())
        .finalize()
        .into_bytes()
        .to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_html_and_incomplete_responses_without_panicking() {
        assert!(
            parse_auth_response(b"<html>Service unavailable</html>", "https://gsa.apple.com")
                .unwrap_err()
                .to_string()
                .contains("anisette")
        );
        assert!(
            parse_auth_response(
                b"<?xml version=\"1.0\"?><plist version=\"1.0\"><dict/></plist>",
                "test"
            )
            .is_err()
        );
        assert!(check_error(&plist::Dictionary::new()).is_err());
    }

    #[test]
    fn preserves_apple_error_code_when_message_is_missing() {
        let status = plist::Dictionary::from_iter([("ec", plist::Value::from(-22406i64))]);
        assert!(matches!(
            check_error(&status),
            Err(Error::AuthSrpWithMessage(-22406, _))
        ));
    }
}
