mod bundle;
mod cgbi;
mod device;
mod device_discovery;
mod device_worker;
mod network_connection;
mod network_discovery;
mod options;
mod package;
mod signer;
mod tweak;
mod xkit_bridge;

use std::path::Path;

pub use bundle::{Bundle, BundleType}; // Bundle helper
pub use device::{
    Device, REMOTE_PAIRING_CACHE_PREFIX, REMOTE_PAIRING_CACHE_SUFFIX, get_device_for_id,
    install_app_mac,
}; // Device helper
pub use device_discovery::{DeviceEvent, watch_devices};
pub use network_connection::NetworkConnection;
pub use network_discovery::watch_network_devices;
pub use options::{
    SignerApp, // Supported app types
    SignerAppReal,
    SignerEmbedding,   // Embedding options
    SignerFeatures,    // Feature support options
    SignerInstallMode, // Installation mode
    SignerMode,        // Signing mode
    SignerOptions,     // Main
    TweakInjection,
    TweakLoadPath,
    TweakLoader,
};
pub use package::Package; // Package helper
pub use signer::Signer; // Signer
pub use tweak::Tweak; // Tweak helper
pub use xkit_bridge::{ConfirmRevocation, RegisteredDevice, XkitSigner}; // xkit provisioning and signing

use thiserror::Error as ThisError;
#[derive(Debug, ThisError)]
pub enum Error {
    #[error("Info.plist not found")]
    BundleInfoPlistMissing,
    // Device
    #[error("Bundle failed to rename, make sure its available: {0}")]
    BundleFailedToCopy(String),
    // Tweak
    #[error("Invalid tweak file path")]
    TweakInvalidPath,
    #[error("Tweak extraction failed: {0}")]
    TweakExtractionFailed(String),
    #[error("Unsupported file type: {0}")]
    UnsupportedFileType(String),

    #[error("Zip error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("Info.plist not found")]
    PackageInfoPlistMissing,
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Plist error: {0}")]
    Plist(#[from] plist::Error),
    #[error("Core error: {0}")]
    Core(#[from] plume_core::Error),
    #[error("Idevice error: {0}")]
    Idevice(#[from] idevice::IdeviceError),
    #[error("Codesign error: {0}")]
    Codesign(#[from] Box<plume_core::AppleCodesignError>),
    #[error("Xkit error: {0}")]
    Xkit(#[from] xkit::Error),
    #[error("Other error: {0}")]
    Other(String),
    #[error("Image error: {0}")]
    Image(#[from] image::ImageError),
}

impl From<plume_core::AppleCodesignError> for Error {
    fn from(error: plume_core::AppleCodesignError) -> Self {
        Self::Codesign(Box::new(error))
    }
}

pub trait PlistInfoTrait {
    fn get_name(&self) -> Option<String>;
    fn get_executable(&self) -> Option<String>;
    fn get_bundle_identifier(&self) -> Option<String>;
    fn get_bundle_name(&self) -> Option<String>;
    fn get_version(&self) -> Option<String>;
    fn get_build_version(&self) -> Option<String>;
}

pub async fn copy_dir_recursively(src: &Path, dst: &Path) -> Result<(), Error> {
    use tokio::fs;

    fs::create_dir_all(dst).await?;
    let mut entries = fs::read_dir(src).await?;

    while let Some(entry) = entries.next_entry().await? {
        let file_type = entry.file_type().await?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if file_type.is_symlink() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::symlink;
                let target = fs::read_link(&src_path).await?;
                symlink(&target, &dst_path)?;
            }
        } else if file_type.is_dir() {
            Box::pin(copy_dir_recursively(&src_path, &dst_path)).await?;
        } else if file_type.is_file() {
            fs::copy(&src_path, &dst_path).await?;
        }
    }

    Ok(())
}
