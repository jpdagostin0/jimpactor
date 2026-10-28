use omnisette::AnisetteConfiguration;
use plist::{Dictionary, Value};
use reqwest::header::{HeaderMap, HeaderValue};
use sha2::{Digest, Sha256};
use srp::groups::G2048;

use crate::Error;

use crate::auth::account::{check_error, parse_response};
use crate::auth::anisette_data::AnisetteData;
use crate::auth::{
    Account, ChallengeRequest, ChallengeRequestBody, GSA_ENDPOINT, InitRequest, InitRequestBody,
    LoginState, RequestHeader, TrustedPhoneNumber, TwoFactorAction, TwoFactorMethod,
    TwoFactorRequest,
};

#[macro_export]
macro_rules! plist_get_string {
    ($base:expr, $( $path:literal )+, $final_key:literal) => {{
        let mut current_val = $base;
        $(
            current_val = current_val
                .get($path)
                .expect(concat!("Missing dictionary key: ", $path))
                .as_dictionary()
                .expect(concat!("Key value is not a dictionary: ", $path));
        )+
        current_val
            .get($final_key)
            .expect(concat!("Missing string key: ", $final_key))
            .as_string()
            .expect(concat!("Value is not a string: ", $final_key))
            .to_string()
    }};

    ($base:expr, $key:literal) => {{
        $base
            .get($key)
            .expect(concat!("Missing key: ", $key))
            .as_string()
            .expect(concat!("Value is not a string: ", $key))
            .to_string()
    }};
}

impl Account {
    pub async fn login(
        appleid_closure: impl Fn() -> Result<(String, String), String>,
        tfa_closure: impl Fn(TwoFactorRequest) -> Result<TwoFactorAction, String>,
        config: AnisetteConfiguration,
    ) -> Result<Account, Error> {
        let anisette = AnisetteData::new(config).await?;
        Account::login_with_anisette(appleid_closure, tfa_closure, anisette).await
    }

    pub async fn login_with_anisette<
        F: Fn() -> Result<(String, String), String>,
        G: Fn(TwoFactorRequest) -> Result<TwoFactorAction, String>,
    >(
        appleid_closure: F,
        tfa_closure: G,
        anisette: AnisetteData,
    ) -> Result<Account, Error> {
        let mut _self = Account::new_with_anisette(anisette)?;
        let (username, password) = appleid_closure().map_err(|e| {
            Error::AuthSrpWithMessage(0, format!("Failed to get Apple ID credentials: {}", e))
        })?;

        let mut response = _self.login_email_pass(&username, &password).await?;
        // Cached so the caller can be offered an SMS fallback even when a trusted
        // device handled the push. Fetched lazily the first time 2FA is needed.
        let mut trusted_phone_numbers: Vec<TrustedPhoneNumber> = Vec::new();

        loop {
            match response {
                LoginState::NeedsDevice2FA => {
                    let extras = _self.get_auth_extras().await.ok();
                    if let Some(extras) = extras {
                        trusted_phone_numbers = extras.trusted_phone_numbers;
                        response = match extras.new_state {
                            Some(state) => state,
                            None => _self.send_2fa_to_devices().await?,
                        };
                    } else {
                        response = _self.send_2fa_to_devices().await?;
                    }
                }
                LoginState::Needs2FAVerification => {
                    if trusted_phone_numbers.is_empty()
                        && let Ok(extras) = _self.get_auth_extras().await
                    {
                        trusted_phone_numbers = extras.trusted_phone_numbers;
                        if let Some(state) = extras.new_state {
                            response = state;
                            continue;
                        }
                    }
                    let request = TwoFactorRequest {
                        method: TwoFactorMethod::Device,
                        trusted_phone_numbers: trusted_phone_numbers.clone(),
                    };
                    match tfa_closure(request).map_err(|e| {
                        Error::AuthSrpWithMessage(0, format!("Failed to get 2FA code: {}", e))
                    })? {
                        TwoFactorAction::SubmitCode(code) => {
                            response = _self.verify_2fa(code).await?
                        }
                        TwoFactorAction::SendSms(id) => {
                            response = _self.send_sms_2fa_to_devices(id).await?
                        }
                    }
                }
                LoginState::NeedsSMS2FA => {
                    let extras = _self.get_auth_extras().await?;
                    trusted_phone_numbers = extras.trusted_phone_numbers;
                    response = match extras.new_state {
                        Some(state) => state,
                        None => match trusted_phone_numbers.first() {
                            Some(phone) => _self.send_sms_2fa_to_devices(phone.id).await?,
                            None => _self.send_2fa_to_devices().await?,
                        },
                    };
                }
                LoginState::NeedsSMS2FAVerification(body) => {
                    let request = TwoFactorRequest {
                        method: TwoFactorMethod::Sms,
                        trusted_phone_numbers: trusted_phone_numbers.clone(),
                    };
                    match tfa_closure(request).map_err(|e| {
                        Error::AuthSrpWithMessage(0, format!("Failed to get SMS 2FA code: {}", e))
                    })? {
                        TwoFactorAction::SubmitCode(code) => {
                            response = _self.verify_sms_2fa(code, body).await?
                        }
                        TwoFactorAction::SendSms(id) => {
                            response = _self.send_sms_2fa_to_devices(id).await?
                        }
                    }
                }
                LoginState::NeedsLogin => {
                    response = _self.login_email_pass(&username, &password).await?
                }
                LoginState::LoggedIn => return Ok(_self),
                LoginState::NeedsExtraStep(step) => {
                    if _self.get_pet().is_some() {
                        return Ok(_self);
                    } else {
                        return Err(Error::ExtraStep(step));
                    }
                }
            }
        }
    }

    pub async fn login_email_pass(
        &mut self,
        username: &str,
        password: &str,
    ) -> Result<LoginState, Error> {
        let username_for_spd = username.to_string().to_lowercase();
        let srp_client = srp::Client::<G2048, Sha256>::new_with_options(false);
        let a: Vec<u8> = (0..32).map(|_| rand::random::<u8>()).collect();
        let a_pub = srp_client.compute_public_ephemeral(&a);

        let anisette = self.get_anisette().await?;

        let mut gsa_headers = HeaderMap::new();
        gsa_headers.insert(
            "Content-Type",
            HeaderValue::from_str("text/x-xml-plist").unwrap(),
        );
        gsa_headers.insert("Accept", HeaderValue::from_str("*/*").unwrap());
        gsa_headers.insert(
            "User-Agent",
            HeaderValue::from_str(anisette.user_agent())
                .map_err(|_| Error::AuthenticationResponse("Invalid anisette user agent".into()))?,
        );
        gsa_headers.insert(
            "X-MMe-Client-Info",
            HeaderValue::from_str(&anisette.get_header("x-mme-client-info")?).map_err(|_| {
                Error::AuthenticationResponse("Invalid anisette client-info header".into())
            })?,
        );

        let header = RequestHeader {
            version: "1.0.1".to_string(),
        };
        let init_body = InitRequestBody {
            a_pub: plist::Value::Data(a_pub),
            cpd: anisette.to_plist(true, false, false),
            operation: "init".to_string(),
            ps: vec!["s2k".to_string(), "s2k_fo".to_string()],
            username: username_for_spd.clone(),
        };

        let init_packet = InitRequest {
            header: header.clone(),
            request: init_body,
        };

        let mut buffer = Vec::new();
        plist::to_writer_xml(&mut buffer, &init_packet)?;

        let res = self
            .client
            .post(GSA_ENDPOINT)
            .headers(gsa_headers.clone())
            .body(buffer)
            .send()
            .await;

        let res = parse_response(res).await?;
        check_error(&res)?;

        let salt = res
            .get("s")
            .and_then(Value::as_data)
            .ok_or_else(|| missing("s"))?;
        let b_pub = res
            .get("B")
            .and_then(Value::as_data)
            .ok_or_else(|| missing("B"))?;
        let iters = res
            .get("i")
            .and_then(Value::as_signed_integer)
            .filter(|i| (1..=10_000_000).contains(i))
            .ok_or_else(|| missing("i"))?;
        let c = res
            .get("c")
            .and_then(Value::as_string)
            .ok_or_else(|| missing("c"))?;

        let hashed_password = Sha256::digest(password.as_bytes());

        let mut password_buf = [0u8; 32];
        pbkdf2::pbkdf2::<hmac::Hmac<Sha256>>(
            &hashed_password,
            salt,
            iters as u32,
            &mut password_buf,
        )?;

        let verifier = srp_client
            .process_reply(&a, username_for_spd.as_bytes(), &password_buf, salt, b_pub)
            .map_err(|_| Error::AuthenticationResponse("Invalid SRP server challenge".into()))?;

        let challenge_body = ChallengeRequestBody {
            m: plist::Value::Data(verifier.proof().to_vec()),
            c: c.to_string(),
            cpd: anisette.to_plist(true, false, false),
            operation: "complete".to_string(),
            username: username_for_spd.clone(),
        };

        let challenge_packet = ChallengeRequest {
            header,
            request: challenge_body,
        };

        let mut buffer = Vec::new();
        plist::to_writer_xml(&mut buffer, &challenge_packet)?;

        gsa_headers.insert("Connection", HeaderValue::from_static("close"));

        let res = self
            .client
            .post(GSA_ENDPOINT)
            .headers(gsa_headers)
            .body(buffer)
            .send()
            .await;

        let res = parse_response(res).await?;
        check_error(&res)?;

        let m2 = res
            .get("M2")
            .and_then(Value::as_data)
            .ok_or_else(|| missing("M2"))?;
        verifier
            .verify_server(m2)
            .map_err(|_| Error::AuthenticationResponse("SRP server verification failed".into()))?;

        let spd_encrypted = res
            .get("spd")
            .and_then(Value::as_data)
            .ok_or_else(|| missing("spd"))?;
        let spd_decrypted = super::decrypt_cbc(&verifier, spd_encrypted)?;
        let mut spd: Dictionary = plist::from_bytes(&spd_decrypted)?;

        if !spd.contains_key("appleId") {
            spd.insert(
                "appleId".to_string(),
                plist::Value::String(username_for_spd),
            );
        }

        self.spd = Some(spd);

        let status = res
            .get("Status")
            .and_then(Value::as_dictionary)
            .ok_or_else(|| missing("Status"))?;
        if let Some(Value::String(auth_type)) = status.get("au") {
            return match auth_type.as_str() {
                "trustedDeviceSecondaryAuth" => Ok(LoginState::NeedsDevice2FA),
                "secondaryAuth" => Ok(LoginState::NeedsSMS2FA),
                other => Ok(LoginState::NeedsExtraStep(other.to_string())),
            };
        }

        if status.get("hsc").and_then(|v| v.as_signed_integer()) == Some(409) {
            return Ok(LoginState::Needs2FAVerification);
        }

        Ok(LoginState::LoggedIn)
    }

    pub fn get_pet(&self) -> Option<String> {
        self.spd
            .as_ref()?
            .get("t")?
            .as_dictionary()?
            .get("com.apple.gs.idms.pet")?
            .as_dictionary()?
            .get("token")?
            .as_string()
            .map(str::to_owned)
    }

    pub fn get_name(&self) -> (String, String) {
        let name = |key| {
            self.spd
                .as_ref()
                .and_then(|d| d.get(key))
                .and_then(Value::as_string)
                .unwrap_or_default()
                .to_owned()
        };
        (name("fn"), name("ln"))
    }

    pub async fn get_anisette(&self) -> Result<AnisetteData, Error> {
        let mut locked = self.anisette.lock().await;
        if locked.needs_refresh() {
            *locked = locked.refresh().await?;
        }
        Ok(locked.clone())
    }
}

fn missing(field: &str) -> Error {
    Error::AuthenticationResponse(format!("Missing or invalid {field} in SRP response"))
}
