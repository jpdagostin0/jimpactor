//! A library to generate "anisette" data. Docs are coming soon.
//!
//! If you want an async API, enable the `async` feature.
//!
//! If you want remote anisette, make sure the `remote-anisette` feature is enabled. (it's currently on by default)

use crate::adi_proxy::ADIProxyAnisetteProvider;
use crate::anisette_headers_provider::AnisetteHeadersProvider;
use adi_proxy::ADIError;
use std::io;
use std::path::PathBuf;
use thiserror::Error;

pub mod adi_libraries;
pub mod adi_proxy;
pub mod anisette_headers_provider;
pub mod remote_anisette_v1;
pub mod store_services_core;

#[cfg(feature = "remote-anisette-v3")]
pub mod remote_anisette_v3;
#[cfg(feature = "remote-anisette-v3")]
pub mod remote_probe;
#[cfg(feature = "remote-anisette-v3")]
pub mod remote_servers;

#[allow(dead_code)]
pub struct AnisetteHeaders;

#[allow(dead_code)]
#[derive(Debug, Error)]
pub enum AnisetteError {
    #[allow(dead_code)]
    #[error("Unsupported device")]
    UnsupportedDevice,
    #[error("Invalid argument {0}")]
    InvalidArgument(String),
    #[error("Anisette not provisioned!")]
    AnisetteNotProvisioned,
    #[error("Plist serialization error {0}")]
    PlistError(#[from] plist::Error),
    #[error("Request Error {0}")]
    ReqwestError(#[from] reqwest::Error),
    #[cfg(feature = "remote-anisette-v3")]
    #[error("Provisioning socket error {0}")]
    WsError(#[from] tokio_tungstenite::tungstenite::error::Error),
    #[cfg(feature = "remote-anisette-v3")]
    #[error("JSON error {0}")]
    SerdeError(#[from] serde_json::Error),
    #[error("IO error {0}")]
    IOError(#[from] io::Error),
    #[error("ADI error {0}")]
    ADIError(#[from] ADIError),
    #[error("Invalid library format")]
    InvalidLibraryFormat,
    #[error("Misc")]
    Misc,
    #[error("Missing Libraries")]
    MissingLibraries,
    #[error("{0}")]
    Anyhow(#[from] anyhow::Error),
}

pub const DEFAULT_ANISETTE_URL: &str = "https://ani.f1sh.me/";

pub const DEFAULT_ANISETTE_URL_V3: &str = "https://mac-ani.jkcoxson.com";

/// Sub-directory of the configuration path used by [`AnisetteProvider::Fake`].
pub const FAKE_IDENTITY_DIR: &str = "fake";

/// Where anisette data comes from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum AnisetteProvider {
    /// Computed on this machine with Apple's ADI libraries, using a persistent identity.
    Local,
    /// Requested from a remote anisette v3 server.
    #[default]
    Remote,
    /// Computed on this machine with Apple's ADI libraries, using a spoofed,
    /// disposable Mac identity that is not tied to this machine.
    Fake,
}

impl AnisetteProvider {
    pub const ALL: [AnisetteProvider; 3] = [
        AnisetteProvider::Local,
        AnisetteProvider::Remote,
        AnisetteProvider::Fake,
    ];

    /// Whether this provider needs Apple's ADI libraries on disk.
    pub fn needs_libraries(&self) -> bool {
        matches!(self, AnisetteProvider::Local | AnisetteProvider::Fake)
    }
}

#[derive(Clone, Debug)]
pub struct AnisetteConfiguration {
    anisette_url: String,
    anisette_url_v3: String,
    configuration_path: PathBuf,
    macos_serial: String,
    provider: AnisetteProvider,
}

impl Default for AnisetteConfiguration {
    fn default() -> Self {
        AnisetteConfiguration::new()
    }
}

impl AnisetteConfiguration {
    pub fn new() -> AnisetteConfiguration {
        AnisetteConfiguration {
            anisette_url: DEFAULT_ANISETTE_URL.to_string(),
            anisette_url_v3: DEFAULT_ANISETTE_URL_V3.to_string(),
            configuration_path: PathBuf::new(),
            macos_serial: "0".to_string(),
            provider: AnisetteProvider::default(),
        }
    }

    pub fn anisette_url_v3(&self) -> &String {
        &self.anisette_url_v3
    }

    pub fn provider(&self) -> AnisetteProvider {
        self.provider
    }

    pub fn set_anisette_url_v3(mut self, anisette_url_v3: String) -> AnisetteConfiguration {
        self.anisette_url_v3 = anisette_url_v3.trim().trim_end_matches('/').to_string();
        self
    }

    pub fn set_provider(mut self, provider: AnisetteProvider) -> AnisetteConfiguration {
        self.provider = provider;
        self
    }

    pub fn anisette_url(&self) -> &String {
        &self.anisette_url
    }

    pub fn configuration_path(&self) -> &PathBuf {
        &self.configuration_path
    }

    pub fn set_anisette_url(mut self, anisette_url: String) -> AnisetteConfiguration {
        self.anisette_url = anisette_url;
        self
    }

    pub fn set_macos_serial(mut self, macos_serial: String) -> AnisetteConfiguration {
        self.macos_serial = macos_serial;
        self
    }

    pub fn set_configuration_path(mut self, configuration_path: PathBuf) -> AnisetteConfiguration {
        self.configuration_path = configuration_path;
        self
    }
}

pub enum AnisetteHeadersProviderType {
    Local,
    Remote,
}

pub struct AnisetteHeadersProviderRes {
    pub provider: Box<dyn AnisetteHeadersProvider>,
    pub provider_type: AnisetteHeadersProviderType,
}

impl AnisetteHeadersProviderRes {
    pub fn local(provider: Box<dyn AnisetteHeadersProvider>) -> AnisetteHeadersProviderRes {
        AnisetteHeadersProviderRes {
            provider,
            provider_type: AnisetteHeadersProviderType::Local,
        }
    }

    pub fn remote(provider: Box<dyn AnisetteHeadersProvider>) -> AnisetteHeadersProviderRes {
        AnisetteHeadersProviderRes {
            provider,
            provider_type: AnisetteHeadersProviderType::Remote,
        }
    }
}

impl AnisetteHeaders {
    pub fn get_anisette_headers_provider(
        configuration: AnisetteConfiguration,
    ) -> Result<AnisetteHeadersProviderRes, AnisetteError> {
        match configuration.provider {
            AnisetteProvider::Local => {
                AnisetteHeaders::get_ssc_anisette_headers_provider(configuration)
            }
            AnisetteProvider::Fake => {
                AnisetteHeaders::get_fake_anisette_headers_provider(configuration)
            }
            AnisetteProvider::Remote => Ok(AnisetteHeadersProviderRes::remote(Box::new(
                AutoRemoteProvider::new(
                    configuration.anisette_url_v3,
                    configuration.configuration_path.clone(),
                    configuration.macos_serial.clone(),
                ),
            ))),
        }
    }

    pub fn get_ssc_anisette_headers_provider(
        configuration: AnisetteConfiguration,
    ) -> Result<AnisetteHeadersProviderRes, AnisetteError> {
        let config_path = configuration.configuration_path();
        let ssc_adi_proxy = store_services_core::StoreServicesCoreADIProxy::new(config_path)?;
        Ok(AnisetteHeadersProviderRes::local(Box::new(
            ADIProxyAnisetteProvider::new(ssc_adi_proxy, config_path.to_path_buf())?,
        )))
    }

    /// Same ADI libraries as [`AnisetteProvider::Local`], but provisioned in a separate
    /// directory under a randomly generated Mac identity (device id + serial number).
    pub fn get_fake_anisette_headers_provider(
        configuration: AnisetteConfiguration,
    ) -> Result<AnisetteHeadersProviderRes, AnisetteError> {
        let library_path = configuration.configuration_path();
        let identity_path = library_path.join(FAKE_IDENTITY_DIR);
        std::fs::create_dir_all(&identity_path)?;

        let mut ssc_adi_proxy =
            store_services_core::StoreServicesCoreADIProxy::with_custom_provisioning_path(
                library_path,
                &identity_path,
            )?;
        ssc_adi_proxy.set_serial_number(adi_libraries::fake_serial_number(&identity_path)?);

        Ok(AnisetteHeadersProviderRes::local(Box::new(
            ADIProxyAnisetteProvider::new(ssc_adi_proxy, identity_path)?,
        )))
    }
}

/// Remote provider that works with either protocol a server might speak: the
/// classic v1 JSON endpoint (also what Sideloadly's remote uses) or the v3
/// provisioning protocol. It tries v3 first and falls back to v1, remembering
/// whichever answered so later refreshes skip the probe.
enum RemoteInner {
    Undecided,
    V1(remote_anisette_v1::RemoteAnisetteProviderV1),
    #[cfg(feature = "remote-anisette-v3")]
    V3(remote_anisette_v3::RemoteAnisetteProviderV3),
}

pub struct AutoRemoteProvider {
    url: String,
    configuration_path: PathBuf,
    serial: String,
    inner: RemoteInner,
}

impl AutoRemoteProvider {
    pub fn new(url: String, configuration_path: PathBuf, serial: String) -> AutoRemoteProvider {
        AutoRemoteProvider {
            url,
            configuration_path,
            serial,
            inner: RemoteInner::Undecided,
        }
    }
}

#[cfg_attr(feature = "async", async_trait::async_trait)]
impl AnisetteHeadersProvider for AutoRemoteProvider {
    #[cfg_attr(not(feature = "async"), remove_async_await::remove_async_await)]
    async fn get_anisette_headers(
        &mut self,
        skip_provisioning: bool,
    ) -> Result<std::collections::HashMap<String, String>, AnisetteError> {
        match &mut self.inner {
            RemoteInner::V1(p) => return p.get_anisette_headers(skip_provisioning).await,
            #[cfg(feature = "remote-anisette-v3")]
            RemoteInner::V3(p) => return p.get_anisette_headers(skip_provisioning).await,
            RemoteInner::Undecided => {}
        }

        #[cfg(feature = "remote-anisette-v3")]
        {
            let mut v3 = remote_anisette_v3::RemoteAnisetteProviderV3::new(
                self.url.clone(),
                self.configuration_path.clone(),
                self.serial.clone(),
            );
            match v3.get_anisette_headers(skip_provisioning).await {
                Ok(headers) => {
                    self.inner = RemoteInner::V3(v3);
                    return Ok(headers);
                }
                Err(error) => {
                    log::debug!("Remote anisette v3 failed ({error}); trying v1");
                }
            }
        }
        #[cfg(not(feature = "remote-anisette-v3"))]
        let _ = &self.serial;

        let mut v1 = remote_anisette_v1::RemoteAnisetteProviderV1::new(
            self.url.clone(),
            self.configuration_path.clone(),
        );
        match v1.get_anisette_headers(skip_provisioning).await {
            Ok(headers) => {
                log::debug!("Remote server {} speaks anisette v1", self.url);
                self.inner = RemoteInner::V1(v1);
                Ok(headers)
            }
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use log::LevelFilter;
    use simplelog::{ColorChoice, ConfigBuilder, TermLogger, TerminalMode};

    pub fn init_logger() {
        if TermLogger::init(
            LevelFilter::Trace,
            ConfigBuilder::new()
                .set_target_level(LevelFilter::Error)
                .add_filter_allow_str("omnisette")
                .build(),
            TerminalMode::Mixed,
            ColorChoice::Auto,
        )
        .is_ok()
        {}
    }

    #[cfg(not(feature = "async"))]
    #[test]
    #[ignore = "network"]
    fn fetch_anisette_auto() -> Result<()> {
        use crate::{AnisetteConfiguration, AnisetteHeaders};
        use log::info;
        use std::path::PathBuf;

        crate::tests::init_logger();

        let mut provider = AnisetteHeaders::get_anisette_headers_provider(
            AnisetteConfiguration::new()
                .set_configuration_path(PathBuf::new().join("anisette_test")),
        )?;
        info!(
            "Headers: {:?}",
            provider.provider.get_authentication_headers()?
        );
        Ok(())
    }
}
