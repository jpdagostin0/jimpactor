//! Compares versions, and installed apps against what the app sources offer.

use std::cmp::Ordering;

use crate::sources::{AppVersion, SourceApp};
use crate::{SignerApp, SignerAppReal};

/// The first run of dot-separated numbers in `text`, so `SideStore v0.6.2-beta` is `[0, 6, 2]`.
pub fn parse_version(text: &str) -> Option<Vec<u64>> {
    let start = text.find(|c: char| c.is_ascii_digit())?;
    let rest = &text[start..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(rest.len());
    rest[..end]
        .split('.')
        .take_while(|part| !part.is_empty())
        .map(|part| part.parse().ok())
        .collect()
}

/// Whether `candidate` is a later version than `current`. Versions that cannot be read never
/// count as newer, so a release called "Nightly" is not offered as an update.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(candidate), Some(current)) => compare(&candidate, &current) == Ordering::Greater,
        _ => false,
    }
}

/// Compares numerically, treating missing parts as zero so `1.2` equals `1.2.0`.
fn compare(a: &[u64], b: &[u64]) -> Ordering {
    (0..a.len().max(b.len()))
        .map(|i| {
            let part = |version: &[u64]| version.get(i).copied().unwrap_or(0);
            part(a).cmp(&part(b))
        })
        .find(|order| order.is_ne())
        .unwrap_or(Ordering::Equal)
}

/// Letters and digits only, lowercased, so `Live Container` matches `livecontainer`.
fn normalized(text: &str) -> String {
    text.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Signing appends the team to bundle identifiers, so `com.a.app.TEAM` is still `com.a.app`.
fn same_bundle(installed: &str, source: &str) -> bool {
    installed == source
        || installed
            .strip_prefix(source)
            .is_some_and(|rest| rest.starts_with('.'))
}

fn matches_source(bundle_id: &str, name: Option<&str>, source: &SourceApp) -> bool {
    if let Some(source_id) = &source.bundle_id {
        return same_bundle(bundle_id, source_id);
    }
    // GitHub sources only have asset names, which are app names or bundle identifiers.
    same_bundle(bundle_id, &source.name)
        || SignerApp::from_bundle_identifier(Some(bundle_id)).to_string() == source.name
        || name.is_some_and(|name| !name.is_empty() && normalized(name) == normalized(&source.name))
}

/// The newest stable version any source offers for this app.
pub fn newest_source_version<'a>(
    bundle_id: &str,
    name: Option<&str>,
    sources: &'a [SourceApp],
) -> Option<(&'a SourceApp, &'a AppVersion)> {
    sources
        .iter()
        .filter(|source| matches_source(bundle_id, name, source))
        .filter_map(|source| Some((source, source.default_version()?)))
        .max_by(
            |(_, a), (_, b)| match (parse_version(&a.version), parse_version(&b.version)) {
                (Some(a), Some(b)) => compare(&a, &b),
                (Some(_), None) => Ordering::Greater,
                (None, Some(_)) => Ordering::Less,
                (None, None) => Ordering::Equal,
            },
        )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppUpdate {
    pub bundle_id: String,
    pub name: String,
    pub installed_version: String,
    pub app: SourceApp,
    pub version: AppVersion,
}

/// Sideloaded apps for which a source has a newer version than the one installed.
pub fn find_updates(installed: &[SignerAppReal], sources: &[SourceApp]) -> Vec<AppUpdate> {
    installed
        .iter()
        .filter(|app| app.sideloaded)
        .filter_map(|app| {
            let bundle_id = app.bundle_id.as_deref()?;
            let installed_version = app.version.as_deref()?;
            let (source, version) =
                newest_source_version(bundle_id, app.installed_name.as_deref(), sources)?;
            is_newer(&version.version, installed_version).then(|| AppUpdate {
                bundle_id: bundle_id.to_owned(),
                name: app
                    .installed_name
                    .clone()
                    .unwrap_or_else(|| source.name.clone()),
                installed_version: installed_version.to_owned(),
                app: source.clone(),
                version: version.clone(),
            })
        })
        .collect()
}

/// What is worth saying about an IPA before it is installed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InstallCheck {
    /// The version on the device, when it has this app.
    pub installed: Option<String>,
    /// The IPA is older than what the device already has.
    pub downgrade: bool,
    /// A newer version from a source, and the source's name.
    pub newer: Option<(String, String)>,
}

/// Compares an IPA with the device and the sources.
pub fn check_install(
    bundle_id: &str,
    name: Option<&str>,
    version: Option<&str>,
    installed: &[SignerAppReal],
    sources: &[SourceApp],
) -> InstallCheck {
    let current = installed
        .iter()
        .find(|app| {
            app.bundle_id
                .as_deref()
                .is_some_and(|id| same_bundle(id, bundle_id))
        })
        .and_then(|app| app.version.clone());
    let downgrade = matches!(
        (&current, version),
        (Some(current), Some(version)) if is_newer(current, version)
    );
    // Against the newest of what is being installed and what is already there.
    let baseline = match (version, current.as_deref()) {
        (Some(version), Some(current)) if is_newer(current, version) => Some(current),
        (Some(version), _) => Some(version),
        (None, current) => current,
    };
    let newer = newest_source_version(bundle_id, name, sources).and_then(|(source, available)| {
        let newer = baseline.is_some_and(|baseline| is_newer(&available.version, baseline));
        newer.then(|| (available.version.clone(), source.source_name.clone()))
    });
    InstallCheck {
        installed: current,
        downgrade,
        newer,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(name: &str, bundle_id: Option<&str>, versions: &[&str]) -> SourceApp {
        SourceApp {
            name: name.into(),
            source_name: name.into(),
            bundle_id: bundle_id.map(str::to_owned),
            versions: versions
                .iter()
                .map(|version| AppVersion {
                    version: (*version).into(),
                    download_url: format!("https://example.com/{version}.ipa"),
                    prerelease: false,
                })
                .collect(),
        }
    }

    fn installed(id: &str, name: &str, version: &str, sideloaded: bool) -> SignerAppReal {
        SignerAppReal {
            installed_name: Some(name.into()),
            version: Some(version.into()),
            sideloaded,
            ..SignerAppReal::from_bundle_identifier(Some(id))
        }
    }

    #[test]
    fn versions_are_found_inside_release_names() {
        assert_eq!(parse_version("v0.6.2"), Some(vec![0, 6, 2]));
        assert_eq!(parse_version("SideStore 0.6.2-beta.1"), Some(vec![0, 6, 2]));
        assert_eq!(parse_version("1.2."), Some(vec![1, 2]));
        assert_eq!(parse_version("Nightly"), None);
    }

    #[test]
    fn newer_compares_numbers_not_text() {
        assert!(is_newer("1.10", "1.9"));
        assert!(is_newer("v2.0", "1.99.99"));
        assert!(!is_newer("1.2", "1.2.0"));
        assert!(!is_newer("1.1", "1.2"));
        assert!(!is_newer("Nightly", "1.0"));
        assert!(!is_newer("1.0", "unknown"));
    }

    #[test]
    fn team_suffixed_apps_match_their_source() {
        let sources = [source("Foo", Some("com.example.foo"), &["2.0", "1.0"])];
        let updates = find_updates(
            &[
                installed("com.example.foo.TEAM12345", "Foo", "1.5", true),
                installed("com.example.foobar", "Other", "1.0", true),
            ],
            &sources,
        );
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].version.version, "2.0");
        assert_eq!(updates[0].installed_version, "1.5");
    }

    #[test]
    fn current_and_app_store_apps_are_not_updates() {
        let sources = [source("Foo", None, &["2.0"])];
        assert!(find_updates(&[installed("a.foo", "Foo", "2.0", true)], &sources).is_empty());
        assert!(find_updates(&[installed("a.foo", "Foo", "1.0", false)], &sources).is_empty());
        assert_eq!(
            find_updates(&[installed("a.foo", "F O O", "1.0", true)], &sources).len(),
            1
        );
    }

    #[test]
    fn the_newest_of_several_sources_wins() {
        let sources = [
            source("Foo", Some("a.foo"), &["1.5"]),
            source("Mirror", Some("a.foo"), &["1.7"]),
        ];
        let (app, version) = newest_source_version("a.foo", None, &sources).unwrap();
        assert_eq!(app.name, "Mirror");
        assert_eq!(version.version, "1.7");
    }

    #[test]
    fn install_checks_report_downgrades_and_newer_sources() {
        let device = [installed("a.foo.TEAM", "Foo", "1.5", true)];
        let sources = [source("Foo", Some("a.foo"), &["2.0"])];

        let old = check_install("a.foo", Some("Foo"), Some("1.0"), &device, &sources);
        assert_eq!(old.installed.as_deref(), Some("1.5"));
        assert!(old.downgrade);
        assert_eq!(old.newer, Some(("2.0".into(), "Foo".into())));

        let latest = check_install("a.foo", Some("Foo"), Some("2.0"), &device, &sources);
        assert!(!latest.downgrade);
        assert_eq!(latest.newer, None);

        let new_app = check_install("b.bar", Some("Bar"), Some("1.0"), &device, &sources);
        assert_eq!(new_app, InstallCheck::default());
    }
}
