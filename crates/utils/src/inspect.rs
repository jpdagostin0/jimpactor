//! Reads what an IPA contains without unpacking or changing it.

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::time::SystemTime;

use goblin::mach::cputype::get_arch_name_from_types;
use goblin::mach::load_command::CommandVariant;
use goblin::mach::{Mach, MachO as GoblinMachO, SingleArch};
use plist::{Dictionary, Value};
use serde::Serialize;
use zip::ZipArchive;

use crate::device_tools::InstalledProfile;
use crate::{Error, FREE_APP_ID_LIMIT};

/// `NSCameraUsageDescription`-style keys are what the app may ask the user for.
const PERMISSION_SUFFIX: &str = "UsageDescription";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct EmbeddedProfile {
    pub name: String,
    pub team: Option<String>,
    pub app_id: Option<String>,
    pub expiration: Option<SystemTime>,
    pub expired: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct AppExtension {
    pub name: String,
    pub bundle_id: Option<String>,
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct IpaReport {
    pub name: Option<String>,
    pub bundle_id: Option<String>,
    pub version: Option<String>,
    pub build: Option<String>,
    pub executable: Option<String>,
    pub minimum_os: Option<String>,
    /// iPhone, iPad and so on.
    pub families: Vec<String>,
    pub archive_size: u64,
    pub unpacked_size: u64,
    pub files: usize,
    pub architectures: Vec<String>,
    /// `None` when the executable could not be read.
    pub encrypted: Option<bool>,
    pub entitlements: Vec<String>,
    pub profile: Option<EmbeddedProfile>,
    pub frameworks: Vec<String>,
    /// Libraries the executable loads from outside the system, such as injected tweaks.
    pub linked_libraries: Vec<String>,
    pub extensions: Vec<AppExtension>,
    pub url_schemes: Vec<String>,
    /// Permission keys with the reason the app gives.
    pub permissions: Vec<(String, String)>,
    /// Problems worth knowing before signing.
    pub warnings: Vec<String>,
}

fn string(plist: &Dictionary, key: &str) -> Option<String> {
    plist.get(key).and_then(Value::as_string).map(str::to_owned)
}

fn read_entry(archive: &mut ZipArchive<File>, name: &str) -> Option<Vec<u8>> {
    let mut entry = archive.by_name(name).ok()?;
    let mut data = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut data).ok()?;
    Some(data)
}

/// The `Payload/Name.app/` folder, found by its top-level Info.plist.
fn app_folder(entries: &[String]) -> Option<String> {
    entries.iter().find_map(|entry| {
        let folder = entry.strip_suffix("Info.plist")?;
        (folder.starts_with("Payload/")
            && folder.ends_with(".app/")
            && folder.matches('/').count() == 2)
            .then(|| folder.to_owned())
    })
}

fn families(plist: &Dictionary) -> Vec<String> {
    plist
        .get("UIDeviceFamily")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_unsigned_integer)
        .map(|family| match family {
            1 => "iPhone".to_owned(),
            2 => "iPad".to_owned(),
            3 => "Apple TV".to_owned(),
            4 => "Apple Watch".to_owned(),
            other => format!("Family {other}"),
        })
        .collect()
}

fn url_schemes(plist: &Dictionary) -> Vec<String> {
    plist
        .get("CFBundleURLTypes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_dictionary)
        .filter_map(|entry| entry.get("CFBundleURLSchemes")?.as_array())
        .flatten()
        .filter_map(Value::as_string)
        .map(str::to_owned)
        .collect()
}

fn permissions(plist: &Dictionary) -> Vec<(String, String)> {
    let mut permissions: Vec<_> = plist
        .iter()
        .filter(|(key, _)| key.ends_with(PERMISSION_SUFFIX))
        .map(|(key, value)| {
            let reason = value.as_string().unwrap_or_default().to_owned();
            (key.clone(), reason)
        })
        .collect();
    permissions.sort();
    permissions
}

/// Architecture names, whether any slice is encrypted, and its non-system libraries.
fn read_binary(data: &[u8]) -> Option<(Vec<String>, bool, Vec<String>)> {
    let mut architectures = Vec::new();
    let mut encrypted = false;
    let mut libraries = Vec::new();
    let mut add = |macho: &GoblinMachO<'_>| {
        let header = macho.header;
        architectures.push(
            get_arch_name_from_types(header.cputype(), header.cpusubtype())
                .unwrap_or("unknown")
                .to_owned(),
        );
        encrypted |= macho
            .load_commands
            .iter()
            .any(|command| match command.command {
                CommandVariant::EncryptionInfo32(info) => info.cryptid != 0,
                CommandVariant::EncryptionInfo64(info) => info.cryptid != 0,
                _ => false,
            });
        for library in macho.libs.iter().skip(1) {
            let system = library.starts_with("/System/") || library.starts_with("/usr/lib/");
            if !system && !libraries.iter().any(|known| known == library) {
                libraries.push((*library).to_owned());
            }
        }
    };
    match Mach::parse(data).ok()? {
        Mach::Binary(macho) => add(&macho),
        Mach::Fat(fat) => {
            for index in 0..fat.narches {
                if let Ok(SingleArch::MachO(macho)) = fat.get(index) {
                    add(&macho);
                }
            }
        }
    }
    Some((architectures, encrypted, libraries))
}

/// The entitlement keys signed into `data`. The signature reader wants a file.
fn read_entitlements(data: &[u8]) -> Vec<String> {
    let path = std::env::temp_dir().join(format!("plume_inspect_{}", uuid::Uuid::new_v4()));
    if std::fs::write(&path, data).is_err() {
        return Vec::new();
    }
    let entitlements = plume_core::MachO::new(&path)
        .ok()
        .and_then(|macho| macho.entitlements().clone());
    let _ = std::fs::remove_file(&path);
    let mut keys: Vec<_> = entitlements
        .into_iter()
        .flat_map(|dictionary| dictionary.into_iter().map(|(key, _)| key))
        .collect();
    keys.sort();
    keys
}

/// Extensions directly inside `app`.
fn read_extensions(
    archive: &mut ZipArchive<File>,
    entries: &[String],
    app: &str,
) -> Vec<AppExtension> {
    let mut extensions = Vec::new();
    for entry in entries {
        let Some(rest) = entry.strip_prefix(app) else {
            continue;
        };
        let Some(name) = rest
            .strip_prefix("PlugIns/")
            .or_else(|| rest.strip_prefix("Extensions/"))
            .and_then(|rest| rest.strip_suffix(".appex/Info.plist"))
            .filter(|name| !name.contains('/'))
        else {
            continue;
        };
        let plist: Option<Dictionary> =
            read_entry(archive, entry).and_then(|data| plist::from_bytes(&data).ok());
        extensions.push(AppExtension {
            name: name.to_owned(),
            bundle_id: plist.as_ref().and_then(|p| string(p, "CFBundleIdentifier")),
            kind: plist
                .as_ref()
                .and_then(|p| p.get("NSExtension")?.as_dictionary())
                .and_then(|p| string(p, "NSExtensionPointIdentifier")),
        });
    }
    extensions.sort_by(|a, b| a.name.cmp(&b.name));
    extensions
}

fn warnings(report: &IpaReport) -> Vec<String> {
    let mut warnings = Vec::new();
    if report.encrypted == Some(true) {
        warnings.push(
            "The executable is encrypted by the App Store, so it will crash when sideloaded. Use a decrypted IPA."
                .to_owned(),
        );
    }
    if report.encrypted.is_none() {
        warnings.push("The executable could not be read.".to_owned());
    }
    if !report.architectures.is_empty()
        && !report
            .architectures
            .iter()
            .any(|arch| arch.starts_with("arm64"))
    {
        warnings
            .push("There is no arm64 code, so this will not run on current devices.".to_owned());
    }
    if report
        .profile
        .as_ref()
        .is_some_and(|profile| profile.expired)
    {
        warnings.push(
            "The embedded profile has expired. This does not matter if you re-sign the app."
                .to_owned(),
        );
    }
    let app_ids = report.extensions.len() as u32 + 1;
    if app_ids >= FREE_APP_ID_LIMIT / 2 {
        warnings.push(format!(
            "Signing with a free account uses {app_ids} of its {FREE_APP_ID_LIMIT} weekly App IDs, one per extension."
        ));
    }
    warnings
}

/// Inspects the IPA at `path`.
pub fn inspect_ipa(path: &Path) -> Result<IpaReport, Error> {
    let file = File::open(path)?;
    let archive_size = file.metadata()?.len();
    let mut archive = ZipArchive::new(file)?;
    let entries: Vec<String> = archive.file_names().map(str::to_owned).collect();
    let app = app_folder(&entries).ok_or(Error::PackageInfoPlistMissing)?;

    let info = read_entry(&mut archive, &format!("{app}Info.plist"))
        .ok_or(Error::PackageInfoPlistMissing)?;
    let info: Dictionary = plist::from_bytes(&info)?;

    let mut report = IpaReport {
        name: string(&info, "CFBundleDisplayName").or_else(|| string(&info, "CFBundleName")),
        bundle_id: string(&info, "CFBundleIdentifier"),
        version: string(&info, "CFBundleShortVersionString"),
        build: string(&info, "CFBundleVersion"),
        executable: string(&info, "CFBundleExecutable"),
        minimum_os: string(&info, "MinimumOSVersion"),
        families: families(&info),
        archive_size,
        url_schemes: url_schemes(&info),
        permissions: permissions(&info),
        ..Default::default()
    };

    for index in 0..archive.len() {
        let entry = archive.by_index_raw(index)?;
        if entry.is_file() {
            report.files += 1;
            report.unpacked_size += entry.size();
        }
    }

    if let Some(data) = report
        .executable
        .as_ref()
        .and_then(|name| read_entry(&mut archive, &format!("{app}{name}")))
    {
        if let Some((architectures, encrypted, libraries)) = read_binary(&data) {
            report.architectures = architectures;
            report.encrypted = Some(encrypted);
            report.linked_libraries = libraries;
        }
        report.entitlements = read_entitlements(&data);
    }

    report.profile = read_entry(&mut archive, &format!("{app}embedded.mobileprovision"))
        .and_then(|data| InstalledProfile::parse(&data))
        .map(|profile| EmbeddedProfile {
            expired: profile.is_expired(),
            name: profile.name,
            team: profile.team_name,
            app_id: profile.app_id,
            expiration: profile.expiration,
        });

    report.frameworks = entries
        .iter()
        .filter_map(|entry| {
            entry
                .strip_prefix(app.as_str())?
                .strip_prefix("Frameworks/")
        })
        .filter_map(|rest| rest.split('/').next())
        .map(str::to_owned)
        .collect();
    report.frameworks.sort();
    report.frameworks.dedup();

    report.extensions = read_extensions(&mut archive, &entries, &app);
    report.warnings = warnings(&report);
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    const MACHO: &[u8] = include_bytes!("../../xkit/tests/fixtures/minimal-arm64.macho");

    fn info_plist() -> Vec<u8> {
        let dict = plist::Dictionary::from_iter([
            ("CFBundleDisplayName", Value::from("Example")),
            ("CFBundleIdentifier", Value::from("com.example.app")),
            ("CFBundleShortVersionString", Value::from("1.2")),
            ("CFBundleVersion", Value::from("34")),
            ("CFBundleExecutable", Value::from("Example")),
            ("MinimumOSVersion", Value::from("15.0")),
            ("NSCameraUsageDescription", Value::from("To scan codes")),
            (
                "UIDeviceFamily",
                Value::Array(vec![Value::from(1u64), Value::from(2u64)]),
            ),
            (
                "CFBundleURLTypes",
                Value::Array(vec![Value::Dictionary(plist::Dictionary::from_iter([(
                    "CFBundleURLSchemes",
                    Value::Array(vec![Value::from("example")]),
                )]))]),
            ),
        ]);
        let mut data = Vec::new();
        Value::Dictionary(dict).to_writer_xml(&mut data).unwrap();
        data
    }

    fn ipa(path: &Path, files: &[(&str, &[u8])]) {
        let mut zip = zip::ZipWriter::new(File::create(path).unwrap());
        for (name, data) in files {
            zip.start_file(*name, SimpleFileOptions::default()).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn a_small_ipa_is_described() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("app.ipa");
        let info = info_plist();
        ipa(
            &path,
            &[
                ("Payload/Example.app/Info.plist", &info),
                ("Payload/Example.app/Example", MACHO),
                ("Payload/Example.app/Frameworks/A.framework/A", b"a"),
                (
                    "Payload/Example.app/Frameworks/A.framework/Info.plist",
                    b"b",
                ),
                ("Payload/Example.app/PlugIns/Share.appex/Info.plist", &info),
            ],
        );

        let report = inspect_ipa(&path).unwrap();
        assert_eq!(report.name.as_deref(), Some("Example"));
        assert_eq!(report.version.as_deref(), Some("1.2"));
        assert_eq!(report.build.as_deref(), Some("34"));
        assert_eq!(report.minimum_os.as_deref(), Some("15.0"));
        assert_eq!(report.families, ["iPhone", "iPad"]);
        assert_eq!(report.architectures, ["arm64"]);
        assert_eq!(report.encrypted, Some(false));
        assert_eq!(report.frameworks, ["A.framework"]);
        assert_eq!(report.extensions.len(), 1);
        assert_eq!(report.extensions[0].name, "Share");
        assert_eq!(report.url_schemes, ["example"]);
        assert_eq!(report.permissions[0].1, "To scan codes");
        assert_eq!(report.files, 5);
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    }

    #[test]
    fn an_unreadable_executable_is_reported_not_fatal() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("app.ipa");
        let info = info_plist();
        ipa(
            &path,
            &[
                ("Payload/Example.app/Info.plist", &info),
                ("Payload/Example.app/Example", b"not a binary"),
            ],
        );
        let report = inspect_ipa(&path).unwrap();
        assert_eq!(report.encrypted, None);
        assert!(!report.warnings.is_empty());
    }

    #[test]
    fn nested_info_plists_do_not_count_as_the_app() {
        let entries = [
            "Payload/A.app/Frameworks/B.framework/Info.plist".to_owned(),
            "Payload/A.app/Info.plist".to_owned(),
        ];
        assert_eq!(app_folder(&entries).as_deref(), Some("Payload/A.app/"));
        assert_eq!(app_folder(&entries[..1]), None);
    }

    #[test]
    fn encrypted_and_many_extension_apps_warn() {
        let report = IpaReport {
            encrypted: Some(true),
            architectures: vec!["armv7".into()],
            extensions: vec![AppExtension::default(); 5],
            ..Default::default()
        };
        let warnings = warnings(&report);
        assert_eq!(warnings.len(), 3);
        assert!(warnings[0].contains("encrypted"));
        assert!(warnings[1].contains("arm64"));
        assert!(warnings[2].contains("6 of its 10"));
    }
}
