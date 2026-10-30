//! Everyday device management: uninstalling apps, provisioning profiles, device
//! status, power, and crash reports. None of these need Developer Mode or a
//! developer disk image.

use std::path::Path;
use std::time::{Duration, SystemTime};

use idevice::IdeviceService;
use idevice::crashreportcopymobile::{CrashReportCopyMobileClient, flush_reports};
use idevice::diagnostics_relay::DiagnosticsRelayClient;
use idevice::installation_proxy::InstallationProxyClient;
use idevice::lockdown::LockdownClient;
use idevice::misagent::MisagentClient;
use idevice::provider::IdeviceProvider;
use plist::{Dictionary, Value};

use crate::Error;
use crate::device::{CONNECTION_LABEL, Device, INSTALLATION_LABEL};

const DIAGNOSTICS_LABEL: &str = "plume_diagnostics";
const CRASH_REPORTS_LABEL: &str = "plume_crash_reports";
// `Retired/`, `Panics/` and friends sit one level down; nothing useful is deeper.
const CRASH_FOLDER_DEPTH: usize = 2;

#[derive(Debug, Clone)]
pub struct InstalledProfile {
    pub uuid: String,
    pub name: String,
    /// Bundle identifier (or wildcard) the profile signs for, without the team prefix.
    pub app_id: Option<String>,
    pub team_name: Option<String>,
    pub expiration: Option<SystemTime>,
}

impl InstalledProfile {
    pub fn is_expired(&self) -> bool {
        self.expiration
            .is_some_and(|date| date <= SystemTime::now())
    }

    fn parse(data: &[u8]) -> Option<Self> {
        let start = data.windows(6).position(|w| w == b"<plist")?;
        let end = data.windows(8).rposition(|w| w == b"</plist>")? + 8;
        let plist = Value::from_reader_xml(data.get(start..end)?).ok()?;
        let dict = plist.as_dictionary()?;
        let string = |key: &str| dict.get(key).and_then(Value::as_string).map(str::to_owned);

        let app_id = dict
            .get("Entitlements")
            .and_then(Value::as_dictionary)
            .and_then(|e| e.get("application-identifier"))
            .and_then(Value::as_string)
            .map(|id| id.split_once('.').map_or(id, |(_, rest)| rest).to_owned());

        Some(Self {
            uuid: string("UUID")?,
            name: string("Name").unwrap_or_default(),
            app_id,
            team_name: string("TeamName"),
            expiration: dict
                .get("ExpirationDate")
                .and_then(Value::as_date)
                .map(SystemTime::from),
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct DeviceDetails {
    pub name: Option<String>,
    /// Hardware identifier, e.g. `iPhone15,2`.
    pub product_type: Option<String>,
    pub ios_version: Option<String>,
    pub build_version: Option<String>,
    pub serial_number: Option<String>,
    pub battery_level: Option<u64>,
    pub battery_charging: Option<bool>,
    pub storage_total: Option<u64>,
    pub storage_free: Option<u64>,
}

impl Device {
    pub async fn uninstall_app(&self, bundle_id: &str) -> Result<(), Error> {
        let bundle_id = bundle_id.to_owned();
        self.with_provider(INSTALLATION_LABEL, move |provider| async move {
            let mut ic = InstallationProxyClient::connect(provider.as_ref()).await?;
            ic.uninstall(bundle_id, None).await?;
            Ok(())
        })
        .await
    }

    /// Provisioning profiles installed on the device, soonest expiring first.
    pub async fn installed_profiles(&self) -> Result<Vec<InstalledProfile>, Error> {
        self.with_provider(INSTALLATION_LABEL, |provider| async move {
            let mut client = MisagentClient::connect(provider.as_ref()).await?;
            let mut profiles: Vec<_> = client
                .copy_all()
                .await?
                .iter()
                .filter_map(|data| InstalledProfile::parse(data))
                .collect();
            profiles.sort_by_key(|p| p.expiration);
            Ok(profiles)
        })
        .await
    }

    pub async fn remove_profiles(&self, uuids: Vec<String>) -> Result<(), Error> {
        self.with_provider(INSTALLATION_LABEL, move |provider| async move {
            let mut client = MisagentClient::connect(provider.as_ref()).await?;
            for uuid in uuids {
                client.remove(&uuid).await?;
            }
            Ok(())
        })
        .await
    }

    /// Model, iOS version, battery, and storage. Fields the device does not report stay `None`.
    pub async fn details(&self) -> Result<DeviceDetails, Error> {
        self.with_provider(CONNECTION_LABEL, |provider| async move {
            let mut lockdown = LockdownClient::connect(provider.as_ref()).await?;
            // Battery and disk domains need an authenticated session; basic values do not.
            match provider.get_pairing_file().await {
                Ok(pairing) => {
                    if let Err(error) = lockdown.start_session(&pairing).await {
                        log::debug!("Lockdown session unavailable: {error}");
                    }
                }
                Err(error) => log::debug!("No pairing file for lockdown session: {error}"),
            }

            let domain = async |lockdown: &mut LockdownClient, domain: Option<&str>| match lockdown
                .get_value(None, domain)
                .await
            {
                Ok(Value::Dictionary(dict)) => dict,
                Ok(_) => Dictionary::new(),
                Err(error) => {
                    log::debug!("Could not read lockdown domain {domain:?}: {error}");
                    Dictionary::new()
                }
            };
            let general = domain(&mut lockdown, None).await;
            let battery = domain(&mut lockdown, Some("com.apple.mobile.battery")).await;
            let disk = domain(&mut lockdown, Some("com.apple.disk_usage")).await;

            let string = |dict: &Dictionary, key: &str| {
                dict.get(key).and_then(Value::as_string).map(str::to_owned)
            };
            let number =
                |dict: &Dictionary, key: &str| dict.get(key).and_then(Value::as_unsigned_integer);

            Ok(DeviceDetails {
                name: string(&general, "DeviceName"),
                product_type: string(&general, "ProductType"),
                ios_version: string(&general, "ProductVersion"),
                build_version: string(&general, "BuildVersion"),
                serial_number: string(&general, "SerialNumber"),
                battery_level: number(&battery, "BatteryCurrentCapacity"),
                battery_charging: battery.get("BatteryIsCharging").and_then(Value::as_boolean),
                storage_total: number(&disk, "TotalDiskCapacity"),
                storage_free: number(&disk, "AmountDataAvailable")
                    .or_else(|| number(&disk, "TotalDataAvailable")),
            })
        })
        .await
    }

    pub async fn restart(&self) -> Result<(), Error> {
        self.with_provider(DIAGNOSTICS_LABEL, |provider| async move {
            let mut client = DiagnosticsRelayClient::connect(provider.as_ref()).await?;
            client.restart().await?;
            Ok(())
        })
        .await
    }

    pub async fn shutdown(&self) -> Result<(), Error> {
        self.with_provider(DIAGNOSTICS_LABEL, |provider| async move {
            let mut client = DiagnosticsRelayClient::connect(provider.as_ref()).await?;
            client.shutdown().await?;
            Ok(())
        })
        .await
    }

    /// Every file under the device's crash log folder (Settings → Privacy &
    /// Security → Analytics Data), as paths relative to that folder, newest
    /// first. Pending reports are flushed first so recent crashes show up.
    pub async fn crash_reports(&self) -> Result<Vec<String>, Error> {
        self.with_provider(CRASH_REPORTS_LABEL, |provider| async move {
            flush_crash_reports(provider.as_ref()).await;
            let mut client = CrashReportCopyMobileClient::connect(provider.as_ref()).await?;
            let mut reports = Vec::new();
            let mut pending = vec![(String::new(), 0)];
            while let Some((dir, depth)) = pending.pop() {
                let entries = match client.ls((!dir.is_empty()).then_some(dir.as_str())).await {
                    Ok(entries) => entries,
                    // The root listing must work; a subfolder we cannot read is skipped.
                    Err(error) if !dir.is_empty() => {
                        log::debug!("Could not list crash folder {dir}: {error}");
                        continue;
                    }
                    Err(error) => return Err(error.into()),
                };
                for name in entries {
                    if name.is_empty() || name.starts_with('.') {
                        continue;
                    }
                    let path = if dir.is_empty() {
                        name
                    } else {
                        format!("{dir}/{name}")
                    };
                    // Folders such as `Retired` or `Panics` have no extension; only
                    // those are worth a file-info round trip.
                    let maybe_dir = depth < CRASH_FOLDER_DEPTH && !name_has_extension(&path);
                    if maybe_dir
                        && client
                            .afc_client
                            .get_file_info(format!("/{path}"))
                            .await
                            .is_ok_and(|info| info.st_ifmt == "S_IFDIR")
                    {
                        pending.push((path, depth + 1));
                    } else {
                        reports.push(path);
                    }
                }
            }
            // Names end in a `YYYY-MM-DD-HHMMSS` timestamp, so that orders them by time.
            reports.sort_by(|a, b| report_timestamp(b).cmp(report_timestamp(a)));
            Ok(reports)
        })
        .await
    }

    /// Reads one crash report, by a path returned from [`Self::crash_reports`].
    pub async fn crash_report(&self, name: &str) -> Result<Vec<u8>, Error> {
        let name = name.to_owned();
        self.with_provider(CRASH_REPORTS_LABEL, move |provider| async move {
            let mut client = CrashReportCopyMobileClient::connect(provider.as_ref()).await?;
            Ok(client.pull(name).await?)
        })
        .await
    }

    /// Copies the named crash reports into `destination`, returning how many were saved.
    pub async fn export_crash_reports(
        &self,
        names: Vec<String>,
        destination: &Path,
    ) -> Result<usize, Error> {
        let destination = destination.to_owned();
        self.with_provider(CRASH_REPORTS_LABEL, move |provider| async move {
            let mut client = CrashReportCopyMobileClient::connect(provider.as_ref()).await?;
            let mut saved = 0;
            for name in names {
                // Report names come from the device; never let one escape the folder.
                let Some(file_name) = Path::new(&name).file_name() else {
                    continue;
                };
                match client.pull(name.as_str()).await {
                    Ok(data) => {
                        std::fs::write(destination.join(file_name), data)
                            .map_err(|e| Error::Other(format!("Could not save {name}: {e}")))?;
                        saved += 1;
                    }
                    Err(error) => log::warn!("Could not copy crash report {name}: {error}"),
                }
            }
            Ok(saved)
        })
        .await
    }
}

async fn flush_crash_reports(provider: &dyn IdeviceProvider) {
    match tokio::time::timeout(Duration::from_secs(10), flush_reports(provider)).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => log::debug!("Crash report flush failed: {error}"),
        Err(_) => log::debug!("Crash report flush timed out"),
    }
}

fn name_has_extension(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.contains('.')
}

/// The last `YYYY-MM-DD-HHMMSS` in the file name, or the name itself. Process
/// names can contain dots and reports can carry several extensions
/// (`.ips.ca.synced`), so the timestamp is found by shape, not position.
fn report_timestamp(path: &str) -> &str {
    const SHAPE: &[u8; 17] = b"0000-00-00-000000";
    let name = path.rsplit('/').next().unwrap_or(path);
    name.as_bytes()
        .windows(SHAPE.len())
        .rposition(|window| {
            window.iter().zip(SHAPE).all(|(&c, &shape)| {
                if shape == b'-' {
                    c == b'-'
                } else {
                    c.is_ascii_digit()
                }
            })
        })
        .map_or(name, |start| &name[start..start + SHAPE.len()])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crash_reports_sort_by_trailing_timestamp() {
        let mut names = vec![
            "Zebra-2026-01-02-101010.ips".to_owned(),
            "Alpha-2026-03-04-090000.ips".to_owned(),
        ];
        names.sort_by(|a, b| report_timestamp(b).cmp(report_timestamp(a)));
        assert_eq!(names[0], "Alpha-2026-03-04-090000.ips");
        assert_eq!(
            report_timestamp("Retired/Foo-2026-01-02-101010.ips.ca.synced"),
            "2026-01-02-101010"
        );
        assert_eq!(
            report_timestamp("com.apple.WebKit.WebContent-2026-05-06-070809.ips"),
            "2026-05-06-070809"
        );
        assert!(name_has_extension("Retired/Foo.ips"));
        assert!(!name_has_extension("Retired"));
    }

    #[test]
    fn profile_parse_strips_team_prefix() {
        let mut entitlements = Dictionary::new();
        entitlements.insert(
            "application-identifier".into(),
            "ABCDE12345.com.example.app".into(),
        );
        let mut dict = Dictionary::new();
        dict.insert("UUID".into(), "1234".into());
        dict.insert("Name".into(), "Example".into());
        dict.insert("Entitlements".into(), Value::Dictionary(entitlements));
        let mut xml = b"\x30\x80junk".to_vec();
        Value::Dictionary(dict).to_writer_xml(&mut xml).unwrap();
        xml.extend_from_slice(b"trailing signature");

        let profile = InstalledProfile::parse(&xml).unwrap();
        assert_eq!(profile.uuid, "1234");
        assert_eq!(profile.app_id.as_deref(), Some("com.example.app"));
        assert!(!profile.is_expired());
    }
}
