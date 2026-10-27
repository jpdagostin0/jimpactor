//! Fetching Apple's ADI libraries (used by local and fake anisette).
//!
//! The libraries ship inside the Apple Music APK. Instead of downloading the
//! whole APK (~140 MB), we read its zip central directory with HTTP range
//! requests and only fetch the two libraries we need.

use std::io::Read;
use std::path::{Path, PathBuf};

use log::{debug, info};
use rand::Rng;
use reqwest::header::{ACCEPT_ENCODING, CONTENT_RANGE, RANGE};

use crate::AnisetteError;

pub const APPLE_MUSIC_APK_URL: &str =
    "https://apps.mzstatic.com/content/android-apple-music-apk/applemusic.apk";

pub const LIBRARY_NAMES: [&str; 2] = ["libstoreservicescore.so", "libCoreADI.so"];

#[cfg(target_arch = "x86_64")]
pub const ANDROID_ARCH: &str = "x86_64";
#[cfg(target_arch = "x86")]
pub const ANDROID_ARCH: &str = "x86";
#[cfg(target_arch = "arm")]
pub const ANDROID_ARCH: &str = "armeabi-v7a";
#[cfg(target_arch = "aarch64")]
pub const ANDROID_ARCH: &str = "arm64-v8a";

const EOCD_SIGNATURE: u32 = 0x06054b50;
const CENTRAL_SIGNATURE: u32 = 0x02014b50;
const LOCAL_SIGNATURE: u32 = 0x04034b50;
const LOCAL_HEADER_LEN: u64 = 30;
// 22 byte EOCD record + a comment of at most 65535 bytes.
const EOCD_SEARCH_LEN: u64 = 22 + 0xFFFF;

/// Directory the libraries for this architecture live in, relative to `configuration_path`.
pub fn libraries_dir(configuration_path: &Path) -> PathBuf {
    configuration_path.join("lib").join(ANDROID_ARCH)
}

pub fn libraries_present(configuration_path: &Path) -> bool {
    let dir = libraries_dir(configuration_path);
    LIBRARY_NAMES.iter().all(|name| dir.join(name).is_file())
}

/// Downloads the ADI libraries into `configuration_path` if they are not there yet.
pub async fn ensure_libraries(configuration_path: &Path) -> Result<(), AnisetteError> {
    if libraries_present(configuration_path) {
        return Ok(());
    }
    download_libraries(configuration_path).await
}

pub async fn download_libraries(configuration_path: &Path) -> Result<(), AnisetteError> {
    info!("Downloading ADI libraries from {APPLE_MUSIC_APK_URL}");
    // Byte ranges must be of the raw file, so don't let the CDN compress responses.
    let client = reqwest::Client::builder().no_gzip().build()?;

    let (tail, total_len) = fetch_tail(&client, EOCD_SEARCH_LEN).await?;
    let eocd = (0..tail.len().saturating_sub(21))
        .rev()
        .find(|&i| read_u32(&tail, i) == EOCD_SIGNATURE)
        .ok_or(AnisetteError::InvalidLibraryFormat)?;
    let cd_size = read_u32(&tail, eocd + 12) as u64;
    let cd_offset = read_u32(&tail, eocd + 16) as u64;
    if cd_offset + cd_size > total_len {
        return Err(AnisetteError::InvalidLibraryFormat);
    }

    let central_directory = fetch_range(&client, cd_offset, cd_size).await?;
    let wanted: Vec<String> = LIBRARY_NAMES
        .iter()
        .map(|name| format!("lib/{ANDROID_ARCH}/{name}"))
        .collect();

    let dir = libraries_dir(configuration_path);
    std::fs::create_dir_all(&dir)?;

    let mut found = 0;
    let mut pos = 0;
    while pos + 46 <= central_directory.len() {
        if read_u32(&central_directory, pos) != CENTRAL_SIGNATURE {
            return Err(AnisetteError::InvalidLibraryFormat);
        }
        let method = read_u16(&central_directory, pos + 10);
        let compressed_size = read_u32(&central_directory, pos + 20) as u64;
        let uncompressed_size = read_u32(&central_directory, pos + 24) as usize;
        let name_len = read_u16(&central_directory, pos + 28) as usize;
        let extra_len = read_u16(&central_directory, pos + 30) as usize;
        let comment_len = read_u16(&central_directory, pos + 32) as usize;
        let local_offset = read_u32(&central_directory, pos + 42) as u64;
        let name = central_directory
            .get(pos + 46..pos + 46 + name_len)
            .ok_or(AnisetteError::InvalidLibraryFormat)?;
        let name = String::from_utf8_lossy(name);
        pos += 46 + name_len + extra_len + comment_len;

        if !wanted.iter().any(|w| *w == name) {
            continue;
        }

        debug!("Extracting {name} ({compressed_size} bytes, method {method})");
        let local_header = fetch_range(&client, local_offset, LOCAL_HEADER_LEN).await?;
        if read_u32(&local_header, 0) != LOCAL_SIGNATURE {
            return Err(AnisetteError::InvalidLibraryFormat);
        }
        let data_offset = local_offset
            + LOCAL_HEADER_LEN
            + read_u16(&local_header, 26) as u64
            + read_u16(&local_header, 28) as u64;
        let compressed = fetch_range(&client, data_offset, compressed_size).await?;

        let data = match method {
            0 => compressed,
            8 => {
                let mut out = Vec::with_capacity(uncompressed_size);
                flate2::read::DeflateDecoder::new(compressed.as_slice()).read_to_end(&mut out)?;
                out
            }
            _ => return Err(AnisetteError::InvalidLibraryFormat),
        };
        if data.len() != uncompressed_size {
            return Err(AnisetteError::InvalidLibraryFormat);
        }

        let file_name = name.rsplit('/').next().unwrap();
        let target = dir.join(file_name);
        let partial = dir.join(format!("{file_name}.part"));
        std::fs::write(&partial, &data)?;
        std::fs::rename(&partial, &target)?;
        found += 1;
    }

    if found != wanted.len() {
        return Err(AnisetteError::MissingLibraries);
    }
    info!("ADI libraries saved to {}", dir.display());
    Ok(())
}

/// Returns the spoofed Mac serial number for a fake identity, generating one on first use.
pub fn fake_serial_number(identity_path: &Path) -> Result<String, AnisetteError> {
    let path = identity_path.join("serial");
    if let Ok(serial) = std::fs::read_to_string(&path) {
        let serial = serial.trim();
        if serial.len() == 12 {
            return Ok(serial.to_string());
        }
    }

    // Mac serials are 12 characters from this alphabet (no I or O).
    const ALPHABET: &[u8] = b"0123456789ABCDEFGHJKLMNPQRSTUVWXYZ";
    let mut rng = rand::thread_rng();
    let serial: String = "C02"
        .chars()
        .chain((0..9).map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char))
        .collect();
    std::fs::write(&path, &serial)?;
    Ok(serial)
}

async fn fetch_tail(client: &reqwest::Client, len: u64) -> Result<(Vec<u8>, u64), AnisetteError> {
    let response = client
        .get(APPLE_MUSIC_APK_URL)
        .header(RANGE, format!("bytes=-{len}"))
        .header(ACCEPT_ENCODING, "identity")
        .send()
        .await?
        .error_for_status()?;
    let total_len = response
        .headers()
        .get(CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.rsplit('/').next())
        .and_then(|v| v.parse::<u64>().ok())
        .ok_or(AnisetteError::InvalidLibraryFormat)?;
    Ok((response.bytes().await?.to_vec(), total_len))
}

async fn fetch_range(
    client: &reqwest::Client,
    offset: u64,
    len: u64,
) -> Result<Vec<u8>, AnisetteError> {
    if len == 0 {
        return Ok(Vec::new());
    }
    let bytes = client
        .get(APPLE_MUSIC_APK_URL)
        .header(RANGE, format!("bytes={}-{}", offset, offset + len - 1))
        .header(ACCEPT_ENCODING, "identity")
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    if bytes.len() as u64 != len {
        return Err(AnisetteError::InvalidLibraryFormat);
    }
    Ok(bytes.to_vec())
}

fn read_u16(buf: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([buf[at], buf[at + 1]])
}

fn read_u32(buf: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]])
}

#[cfg(test)]
mod tests {
    use crate::{AnisetteConfiguration, AnisetteError, AnisetteHeaders, AnisetteProvider};

    async fn headers_for(provider: AnisetteProvider) -> Result<(), AnisetteError> {
        crate::tests::init_logger();
        let path = std::env::temp_dir().join("omnisette_provider_test");
        super::ensure_libraries(&path).await?;
        assert!(super::libraries_present(&path));

        let config = AnisetteConfiguration::new()
            .set_configuration_path(path)
            .set_provider(provider);
        let mut res = AnisetteHeaders::get_anisette_headers_provider(config)?;
        let headers = res.provider.get_authentication_headers().await?;
        for key in [
            "X-Apple-I-MD",
            "X-Apple-I-MD-M",
            "X-Mme-Client-Info",
            "X-Mme-Device-Id",
        ] {
            assert!(headers.contains_key(key), "missing {key}");
        }
        log::info!("{provider:?} generated all required anisette headers");
        Ok(())
    }

    #[tokio::test]
    #[ignore = "network"]
    async fn local_and_fake_headers() -> Result<(), AnisetteError> {
        headers_for(AnisetteProvider::Local).await?;
        headers_for(AnisetteProvider::Fake).await
    }

    #[tokio::test]
    #[ignore = "network"]
    async fn local_headers_from_fresh_setup() -> Result<(), AnisetteError> {
        let path = std::env::temp_dir().join(format!("jimpactor-local-{}", uuid::Uuid::new_v4()));
        assert!(!super::libraries_present(&path));
        super::ensure_libraries(&path).await?;
        assert!(super::libraries_present(&path));
        let config = AnisetteConfiguration::new()
            .set_configuration_path(path.clone())
            .set_provider(AnisetteProvider::Local);
        let mut provider = AnisetteHeaders::get_anisette_headers_provider(config.clone())?;
        let headers = provider.provider.get_authentication_headers().await?;
        for key in ["X-Apple-I-MD", "X-Apple-I-MD-M"] {
            assert!(
                headers.get(key).is_some_and(|value| !value.is_empty()),
                "missing {key}"
            );
        }
        let device_id = headers.get("X-Mme-Device-Id").unwrap().clone();
        drop(provider);
        let mut provider = AnisetteHeaders::get_anisette_headers_provider(config)?;
        let headers = provider.provider.get_authentication_headers().await?;
        assert_eq!(headers.get("X-Mme-Device-Id"), Some(&device_id));
        Ok(())
    }

    #[tokio::test]
    #[ignore = "network"]
    async fn remote_headers() -> Result<(), AnisetteError> {
        headers_for(AnisetteProvider::Remote).await
    }
}
