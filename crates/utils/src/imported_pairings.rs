//! Lockdown pairing files imported by the user, such as ones exported from another
//! computer or made by SideStore, jitterbugpair or Jimpactor itself. Wi-Fi discovery
//! tries them alongside the host's own pairing records, so a device that was never
//! trusted over USB on this computer can still connect.

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

use idevice::lockdown::LockdownClient;
use idevice::pairing_file::PairingFile;
use idevice::remote_pairing::RpPairingFile;

use crate::Error;
use crate::network_connection::Route;

/// A pairing file saved in the imported pairings folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedPairing {
    pub udid: String,
    pub path: PathBuf,
}

/// The result of [`import_pairing_file`].
#[derive(Debug, Clone)]
pub struct ImportResult {
    pub pairing: ImportedPairing,
    /// The device's name, when it was reached at the given address during import.
    pub device_name: Option<String>,
}

/// Where imported pairing files are kept, `<data>/pairings/<UDID>.plist`.
pub fn imported_pairings_dir() -> PathBuf {
    plume_store::data_path().join("pairings")
}

/// Lists the imported pairing files, sorted by UDID.
pub fn list_imported_pairings() -> Vec<ImportedPairing> {
    list_in(&imported_pairings_dir())
}

/// Deletes the imported pairing file for `udid`.
pub fn remove_imported_pairing(udid: &str) -> Result<(), Error> {
    if !is_udid(udid) {
        return Err(Error::Other(format!("{udid} is not a device UDID")));
    }
    std::fs::remove_file(imported_pairings_dir().join(format!("{udid}.plist")))?;
    Ok(())
}

/// Validates and saves a pairing file so Wi-Fi discovery can use it.
///
/// With `address`, the device is authenticated there first: the import fails unless
/// the pairing is accepted, the UDID is taken from the device, and discovery is then
/// asked to connect to that address. Without one, the file must name its UDID.
pub async fn import_pairing_file(
    source: &Path,
    address: Option<IpAddr>,
) -> Result<ImportResult, Error> {
    let bytes = tokio::fs::read(source).await?;
    import_pairing_bytes(&bytes, address, &imported_pairings_dir()).await
}

async fn import_pairing_bytes(
    bytes: &[u8],
    address: Option<IpAddr>,
    dir: &Path,
) -> Result<ImportResult, Error> {
    let mut pairing = PairingFile::from_bytes(bytes).map_err(|error| {
        if RpPairingFile::from_bytes(bytes).is_ok() {
            return Error::Other(
                "This is a remote pairing (RPPairing) file, which cannot connect over Wi-Fi on its own. Import a lockdown pairing file instead, such as the .mobiledevicepairing file from \"Export pairing file\" or a SideStore/jitterbugpair pairing file.".into(),
            );
        }
        Error::Other(format!(
            "This is not a usable lockdown pairing file ({error}). Pick a .plist or .mobiledevicepairing file that contains HostCertificate, HostPrivateKey and DeviceCertificate."
        ))
    })?;
    pairing.udid = pairing.udid.filter(|udid| !udid.is_empty());

    let mut device_name = None;
    if let Some(address) = address {
        let socket = SocketAddr::new(address, LockdownClient::LOCKDOWND_PORT);
        let (device, _heartbeat, _lockdown) = crate::network_discovery::connect_with_pairings(
            &[socket],
            std::slice::from_ref(&pairing),
            0,
            Route::Lan,
        )
        .await
        .map_err(|error| {
            Error::Other(format!(
                "Could not connect to the device at {address} with this pairing file: {error}. Check that the iPhone is unlocked, on the same network, and that the pairing file belongs to it."
            ))
        })?;
        pairing.udid = Some(device.udid);
        device_name = Some(device.name);
    }

    let udid = pairing.udid.clone().ok_or_else(|| {
        Error::Other(
            "This pairing file does not say which device it belongs to. Enter the device's IP address so it can be identified.".into(),
        )
    })?;
    if !is_udid(&udid) {
        return Err(Error::Other(format!(
            "The pairing file names an invalid UDID: {udid}"
        )));
    }

    tokio::fs::create_dir_all(dir).await?;
    let path = dir.join(format!("{udid}.plist"));
    tokio::fs::write(&path, pairing.serialize()?).await?;
    log::info!("Imported pairing file for {udid} to {}", path.display());

    if let Some(address) = address {
        crate::network_discovery::connect_wifi_address(address);
    }

    Ok(ImportResult {
        pairing: ImportedPairing { udid, path },
        device_name,
    })
}

/// UDIDs are 25 characters with a hyphen, or 40 hex characters.
fn is_udid(value: &str) -> bool {
    matches!(value.len(), 25 | 40) && value.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

fn list_in(dir: &Path) -> Vec<ImportedPairing> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut pairings: Vec<_> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            if path
                .extension()
                .is_none_or(|extension| extension != "plist")
            {
                return None;
            }
            let udid = path.file_stem()?.to_str()?.to_owned();
            is_udid(&udid).then_some(ImportedPairing { udid, path })
        })
        .collect();
    pairings.sort_by(|a, b| a.udid.cmp(&b.udid));
    pairings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network_connection::tests::fixture_pairing;

    const UDID: &str = "00008130-0123456789ABCDEF";

    #[tokio::test]
    async fn imports_a_pairing_that_names_its_device_and_lists_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut pairing = fixture_pairing();
        pairing.udid = Some(UDID.into());
        let result = import_pairing_bytes(&pairing.serialize().unwrap(), None, dir.path())
            .await
            .unwrap();
        assert_eq!(result.pairing.udid, UDID);
        assert!(result.device_name.is_none());

        let listed = list_in(dir.path());
        assert_eq!(listed, vec![result.pairing.clone()]);
        let saved = PairingFile::read_from_file(&listed[0].path).unwrap();
        assert_eq!(saved.udid.as_deref(), Some(UDID));
        assert_eq!(saved.host_id, "fixture-host");
        // Discovery reads the folder the same way it reads the host's lockdown records.
        let discovered = crate::network_discovery::read_pairings(dir.path());
        assert_eq!(discovered[0].udid.as_deref(), Some(UDID));
    }

    #[tokio::test]
    async fn rejects_pairings_without_a_udid_or_address_and_non_pairing_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut pairing = fixture_pairing();
        pairing.udid = None;
        let error = import_pairing_bytes(&pairing.serialize().unwrap(), None, dir.path())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("IP address"), "{error}");

        let remote = RpPairingFile::generate("fixture-host").to_bytes();
        let error = import_pairing_bytes(&remote, None, dir.path())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("RPPairing"), "{error}");

        let error = import_pairing_bytes(b"<plist></plist>", None, dir.path())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("not a usable"), "{error}");
        assert!(list_in(dir.path()).is_empty());
    }
}
