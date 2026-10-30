//! Fetch the known Mac CoreADI build directly from Apple's iTunes installer.

use crate::AnisetteError;
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

pub const LIBRARY_SHA256: &str = "2eafc3c79c7f41a8afbfbeaaf23ee5dab5ad5bda55ec8e54dbc0ddf1d20ce630";
const PAYLOAD_SHA256: &str = "8a6c068c95036326eb2d2885fa60912ed0f7229d0939ae92934a70a3bb6cf025";
const URL: &str = "https://secure-appldnld.apple.com/itunes12/041-56365-20210413-79A5AC6D-6B0A-46EC-B399-5A381183E3BE/iTunes12.8.3.dmg";
const OFFSET: u64 = 184_678_769;
const LENGTH: usize = 3_471_595;
const FRAMEWORK: &str = "System/Library/PrivateFrameworks/CoreADI.framework/Versions/A/CoreADI";

pub fn library_path(root: &Path) -> PathBuf {
    root.join("lib/mac/CoreADI")
}

#[cfg_attr(not(feature = "async"), remove_async_await::remove_async_await)]
pub async fn ensure_library(root: &Path) -> Result<(), AnisetteError> {
    let path = library_path(root);
    if std::fs::read(&path).is_ok_and(|bytes| hex::encode(Sha256::digest(bytes)) == LIBRARY_SHA256)
    {
        return Ok(());
    }
    #[cfg(feature = "async")]
    use reqwest::Client;
    #[cfg(not(feature = "async"))]
    use reqwest::blocking::Client;

    log::info!("Downloading Mac CoreADI from Apple's iTunes installer");
    let response = Client::builder()
        .timeout(Duration::from_secs(60))
        .no_gzip()
        .build()?
        .get(URL)
        .header(
            "Range",
            format!("bytes={OFFSET}-{}", OFFSET + LENGTH as u64 - 1),
        )
        .header("Accept-Encoding", "identity")
        .send()
        .await?
        .error_for_status()?;
    if response.status() != reqwest::StatusCode::PARTIAL_CONTENT
        || response.content_length() != Some(LENGTH as u64)
    {
        return Err(
            anyhow::anyhow!("Apple did not return the expected Mac CoreADI byte range").into(),
        );
    }
    let payload = response.bytes().await?;
    let library = extract_library(&payload)?;
    std::fs::create_dir_all(path.parent().unwrap())?;
    let temporary = path.with_extension("partial");
    std::fs::write(&temporary, library)?;
    std::fs::rename(temporary, path)?;
    Ok(())
}

fn extract_library(payload: &[u8]) -> Result<Vec<u8>, AnisetteError> {
    if payload.len() != LENGTH || hex::encode(Sha256::digest(payload)) != PAYLOAD_SHA256 {
        return Err(AnisetteError::InvalidLibraryFormat);
    }
    let mut cpio = Vec::new();
    flate2::read::GzDecoder::new(payload)
        .take(64 * 1024 * 1024)
        .read_to_end(&mut cpio)?;
    let fat = cpio_file(&cpio, FRAMEWORK)?;
    let library = x86_64_slice(fat)?;
    if hex::encode(Sha256::digest(library)) != LIBRARY_SHA256 {
        return Err(AnisetteError::InvalidLibraryFormat);
    }
    Ok(library.to_vec())
}

fn cpio_file<'a>(bytes: &'a [u8], wanted: &str) -> Result<&'a [u8], AnisetteError> {
    let bad = || AnisetteError::InvalidLibraryFormat;
    let octal = |bytes: &[u8]| {
        usize::from_str_radix(std::str::from_utf8(bytes).map_err(|_| bad())?, 8).map_err(|_| bad())
    };
    let mut offset = 0usize;
    loop {
        let header = bytes
            .get(offset..offset.checked_add(76).ok_or_else(bad)?)
            .ok_or_else(bad)?;
        if &header[..6] != b"070707" {
            return Err(bad());
        }
        let name_length = octal(&header[59..65])?;
        let size = octal(&header[65..76])?;
        let name_start = offset + 76;
        let data_start = name_start.checked_add(name_length).ok_or_else(bad)?;
        let name = bytes
            .get(name_start..data_start)
            .and_then(|name| name.strip_suffix(&[0]))
            .ok_or_else(bad)?;
        let name = std::str::from_utf8(name).map_err(|_| bad())?;
        let end = data_start.checked_add(size).ok_or_else(bad)?;
        let data = bytes.get(data_start..end).ok_or_else(bad)?;
        if name.strip_prefix("./").unwrap_or(name) == wanted {
            return Ok(data);
        }
        if name == "TRAILER!!!" {
            return Err(bad());
        }
        offset = end;
    }
}

fn x86_64_slice(bytes: &[u8]) -> Result<&[u8], AnisetteError> {
    let bad = || AnisetteError::InvalidLibraryFormat;
    let word = |offset: usize| -> Result<u32, AnisetteError> {
        let value = bytes
            .get(offset..offset.checked_add(4).ok_or_else(bad)?)
            .ok_or_else(bad)?;
        Ok(u32::from_be_bytes(value.try_into().map_err(|_| bad())?))
    };
    if word(0)? != 0xcafebabe {
        return Err(bad());
    }
    let count = word(4)? as usize;
    if count > bytes.len().saturating_sub(8) / 20 {
        return Err(bad());
    }
    for index in 0..count {
        let entry = 8 + index * 20;
        if word(entry)? == 0x01000007 {
            let start = word(entry + 8)? as usize;
            let size = word(entry + 12)? as usize;
            return bytes
                .get(start..start.checked_add(size).ok_or_else(bad)?)
                .ok_or_else(bad);
        }
    }
    Err(bad())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_installers_and_out_of_bounds_slices_are_rejected() {
        for bytes in [vec![], vec![0; 100], b"070707".to_vec()] {
            assert!(extract_library(&bytes).is_err());
            assert!(cpio_file(&bytes, FRAMEWORK).is_err());
            assert!(x86_64_slice(&bytes).is_err());
        }
        let mut fat = vec![0; 28];
        fat[..4].copy_from_slice(&0xcafebabeu32.to_be_bytes());
        fat[4..8].copy_from_slice(&1u32.to_be_bytes());
        fat[8..12].copy_from_slice(&0x01000007u32.to_be_bytes());
        fat[16..20].copy_from_slice(&u32::MAX.to_be_bytes());
        fat[20..24].copy_from_slice(&100u32.to_be_bytes());
        assert!(x86_64_slice(&fat).is_err());
    }
}
