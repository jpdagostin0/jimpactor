//! Rust implementation of XKit, sharing GrandSlam and anisette with `plume_core`.
//!
//! XKit: Copyright © 2019 Kabir Oberai. All rights reserved.
//! XKit is part of [XTool](https://github.com/xtool-org/xtool), which is
//! distributed under the MIT license. See the crate's `LICENSE` for XTool's
//! MIT notice (Copyright (c) 2024 Kabir Oberai).
//!
//! Network clients and persistent storage are injectable. Device operations use
//! the workspace's native Rust `idevice` implementation on all desktop platforms.

#![doc = include_str!("../README.md")]

pub mod developer_services;
pub mod entitlements;
pub mod grand_slam;
pub mod http;
pub mod installation;
pub mod integration;
pub mod model;
pub mod signer;
pub mod utilities;

pub use developer_services::{
    ASCKey, DeveloperAPIAuthData, DeveloperAPIClient, DeveloperServicesClient,
    DeveloperServicesLoginToken, IdentifierPolicy, XcodeAuthData,
};
pub use entitlements::{Entitlement, Entitlements};
pub use integration::IntegratedInstaller;
pub use model::{Certificate, Keypair, Mobileprovision, PrivateKey, SigningInfo};
pub use signer::{AutoSigner, Signer, SigningContext, TargetDevice};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    InvalidData(String),
    #[error("The operation was cancelled")]
    Cancelled,
    #[error("An installation is already in progress")]
    AlreadyInstalling,
    #[error("Developer API error {code}: {message}")]
    DeveloperApi { code: String, message: String },
    #[error("{method} {url} failed: HTTP {status}: {body}")]
    Http {
        method: String,
        url: String,
        status: u16,
        body: String,
    },
    #[error("This operation requires Xcode authentication")]
    RequiresXcode,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Plist(#[from] plist::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Reqwest(#[from] reqwest::Error),
    #[error(transparent)]
    Core(#[from] plume_core::Error),
    #[error(transparent)]
    Device(#[from] idevice::IdeviceError),
    #[error(transparent)]
    Zip(#[from] zip::result::ZipError),
    #[error(transparent)]
    Certificate(#[from] x509_certificate::X509CertificateError),
    #[error(transparent)]
    Codesign(#[from] Box<apple_codesign::AppleCodesignError>),
}

impl From<apple_codesign::AppleCodesignError> for Error {
    fn from(error: apple_codesign::AppleCodesignError) -> Self {
        Self::Codesign(Box::new(error))
    }
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn invalid(message: impl Into<String>) -> Error {
    Error::InvalidData(message.into())
}
