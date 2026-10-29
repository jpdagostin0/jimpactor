//! App sources: GitHub repositories (IPAs attached to releases) and AltStore-format
//! source feeds. The kind is identified automatically from the URL the user adds.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const SOURCES_FILE: &str = "sources.json";
const USER_AGENT: &str = concat!("Jimpactor/", env!("CARGO_PKG_VERSION"));
pub(crate) const DEFAULT_SOURCE_URL: &str = "https://github.com/SideStore/SideStore";
/// Name of the app the one-click install preselects.
pub(crate) const SIDESTORE: &str = "SideStore";
/// Sources added once for every user after SideStore. Unlike SideStore they can be removed,
/// and a removed one is not added back.
const EXTRA_DEFAULT_SOURCES: [(&str, &str); 3] = [
    ("GoldenNugget", "https://github.com/GoldenNugget-Team/GoldenNugget-mobile"),
    ("LiveContainer", "https://github.com/LiveContainer/LiveContainer"),
    ("StikDebug", "https://github.com/StikDebug/StikDebug"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum SourceKind {
    GitHub,
    AltStore,
}

impl std::fmt::Display for SourceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SourceKind::GitHub => "GitHub",
            SourceKind::AltStore => "AltStore",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Source {
    pub name: String,
    pub url: String,
    pub kind: SourceKind,
}

impl Source {
    pub fn default_source() -> Self {
        Self {
            name: SIDESTORE.to_string(),
            url: DEFAULT_SOURCE_URL.to_string(),
            kind: SourceKind::GitHub,
        }
    }

    pub fn is_default(&self) -> bool {
        self.url == DEFAULT_SOURCE_URL
    }
}

/// The user's sources, stored in `sources.json`. The default SideStore source is always first.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct SourcesStore {
    #[serde(default)]
    sources: Vec<Source>,
    /// Extra default sources already added once, so removing one is remembered.
    #[serde(default)]
    seeded_defaults: Vec<String>,
}

impl SourcesStore {
    fn path(data_path: &Path) -> PathBuf {
        data_path.join(SOURCES_FILE)
    }

    pub fn load(data_path: &Path) -> Self {
        let mut store: Self = std::fs::read_to_string(Self::path(data_path))
            .ok()
            .and_then(|contents| serde_json::from_str(&contents).ok())
            .unwrap_or_default();
        if !store.sources.iter().any(Source::is_default) {
            store.sources.insert(0, Source::default_source());
        }
        store.seed_extra_defaults();
        store
    }

    fn seed_extra_defaults(&mut self) {
        for (name, url) in EXTRA_DEFAULT_SOURCES {
            if self.seeded_defaults.iter().any(|seeded| seeded == url) {
                continue;
            }
            self.seeded_defaults.push(url.to_string());
            if !self.sources.iter().any(|source| source.url == url) {
                self.sources.push(Source {
                    name: name.to_string(),
                    url: url.to_string(),
                    kind: SourceKind::GitHub,
                });
            }
        }
    }

    pub fn save(&self, data_path: &Path) -> Result<(), String> {
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(Self::path(data_path), json).map_err(|e| e.to_string())
    }

    pub fn sources(&self) -> &[Source] {
        &self.sources
    }

    pub fn add(&mut self, source: Source) -> Result<(), String> {
        if self.sources.iter().any(|s| s.url == source.url) {
            return Err(format!("{} is already added", source.url));
        }
        self.sources.push(source);
        Ok(())
    }

    pub fn remove(&mut self, url: &str) {
        self.sources.retain(|s| s.is_default() || s.url != url);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AppVersion {
    pub version: String,
    pub download_url: String,
    pub prerelease: bool,
}

impl std::fmt::Display for AppVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.version)?;
        if self.prerelease {
            f.write_str(" (pre)")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceApp {
    pub name: String,
    pub source_name: String,
    pub bundle_id: Option<String>,
    /// Newest first.
    pub versions: Vec<AppVersion>,
}

impl SourceApp {
    /// The newest stable version, or the newest version when there are only pre-releases.
    pub fn default_version(&self) -> Option<&AppVersion> {
        self.versions
            .iter()
            .find(|v| !v.prerelease)
            .or_else(|| self.versions.first())
    }
}

impl std::fmt::Display for SourceApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.name == self.source_name {
            f.write_str(&self.name)
        } else {
            write!(f, "{} ({})", self.name, self.source_name)
        }
    }
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .map_err(|e| e.to_string())
}

/// `owner/repo` for GitHub repository URLs (or a bare `owner/repo`), `None` otherwise.
fn github_repo(url: &str) -> Option<(String, String)> {
    let trimmed = url.trim().trim_end_matches('/');
    let path = match trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
    {
        Some(rest) => rest
            .strip_prefix("github.com/")
            .or_else(|| rest.strip_prefix("www.github.com/"))?,
        None if !trimmed.contains("://") && trimmed.matches('/').count() == 1 => trimmed,
        None => return None,
    };

    let mut parts = path.split('/');
    let owner = parts.next()?.to_string();
    let repo = parts.next()?.trim_end_matches(".git").to_string();
    // Links to files inside a repo (raw JSON feeds) are AltStore sources, not the repo itself.
    match parts.next() {
        None | Some("releases") | Some("tags") => {}
        Some(_) => return None,
    }
    (!owner.is_empty() && !repo.is_empty()).then_some((owner, repo))
}

/// Works out what kind of source `url` points to and returns it ready to save.
pub(crate) async fn identify(url: &str) -> Result<Source, String> {
    let url = url.trim();
    if let Some((owner, repo)) = github_repo(url) {
        let api = format!("https://api.github.com/repos/{owner}/{repo}");
        let response = client()?
            .get(&api)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !response.status().is_success() {
            return Err(format!(
                "GitHub repository {owner}/{repo} not found ({})",
                response.status()
            ));
        }
        return Ok(Source {
            name: repo.clone(),
            url: format!("https://github.com/{owner}/{repo}"),
            kind: SourceKind::GitHub,
        });
    }

    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("Enter a GitHub repository or an AltStore source URL".to_string());
    }

    let feed = fetch_altstore_feed(url)
        .await
        .map_err(|e| format!("Not a GitHub repository or AltStore source: {e}"))?;
    Ok(Source {
        name: feed.name.unwrap_or_else(|| url.to_string()),
        url: url.to_string(),
        kind: SourceKind::AltStore,
    })
}

/// Lists the apps a source offers.
pub(crate) async fn fetch_apps(source: &Source) -> Result<Vec<SourceApp>, String> {
    match source.kind {
        SourceKind::GitHub => fetch_github_apps(source).await,
        SourceKind::AltStore => {
            let feed = fetch_altstore_feed(&source.url).await?;
            Ok(feed
                .apps
                .into_iter()
                .filter_map(|app| app.into_source_app(&source.name))
                .collect())
        }
    }
}

/// Apps from every source; sources that fail are logged and skipped.
pub(crate) async fn fetch_all(sources: Vec<Source>) -> Vec<SourceApp> {
    let results = futures::future::join_all(sources.iter().map(fetch_apps)).await;
    sources
        .iter()
        .zip(results)
        .flat_map(|(source, result)| {
            result.unwrap_or_else(|e| {
                log::error!("Failed to load source {}: {e}", source.url);
                Vec::new()
            })
        })
        .collect()
}

#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<GitHubAsset>,
}

#[derive(Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
}

async fn fetch_github_apps(source: &Source) -> Result<Vec<SourceApp>, String> {
    let (owner, repo) =
        github_repo(&source.url).ok_or_else(|| format!("Invalid GitHub source {}", source.url))?;
    let response = client()?
        .get(format!(
            "https://api.github.com/repos/{owner}/{repo}/releases?per_page=30"
        ))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("GitHub returned {}", response.status()));
    }
    let releases: Vec<GitHubRelease> = response.json().await.map_err(|e| e.to_string())?;

    // Each distinct IPA file name across releases is one app (e.g. "LiveContainer+SideStore").
    let mut apps: Vec<SourceApp> = Vec::new();
    for release in releases.into_iter().filter(|r| !r.draft) {
        let version = release
            .name
            .filter(|n| !n.trim().is_empty() && n.len() <= 40)
            .unwrap_or(release.tag_name);
        for asset in release.assets {
            let Some(stem) = asset
                .name
                .strip_suffix(".ipa")
                .or_else(|| asset.name.strip_suffix(".tipa"))
            else {
                continue;
            };
            let app_name = strip_version_suffix(stem);
            let entry = AppVersion {
                version: version.clone(),
                download_url: asset.browser_download_url,
                prerelease: release.prerelease,
            };
            match apps.iter_mut().find(|a| a.name == app_name) {
                Some(app) => app.versions.push(entry),
                None => apps.push(SourceApp {
                    name: app_name.to_string(),
                    source_name: source.name.clone(),
                    bundle_id: None,
                    versions: vec![entry],
                }),
            }
        }
    }
    Ok(apps)
}

/// `App_1.2.3` / `App-v1.2` -> `App`, so versioned asset names group into one app.
fn strip_version_suffix(name: &str) -> &str {
    name.rfind(['_', '-'])
        .filter(|&i| {
            let rest = name[i + 1..].strip_prefix('v').unwrap_or(&name[i + 1..]);
            i > 0 && rest.starts_with(|c: char| c.is_ascii_digit())
        })
        .map_or(name, |i| &name[..i])
}

#[derive(Deserialize)]
struct AltStoreFeed {
    #[serde(default)]
    name: Option<String>,
    apps: Vec<AltStoreApp>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AltStoreApp {
    name: String,
    #[serde(default)]
    bundle_identifier: Option<String>,
    #[serde(default)]
    versions: Vec<AltStoreVersion>,
    // Legacy single-version fields.
    #[serde(default)]
    version: Option<String>,
    #[serde(default, rename = "downloadURL")]
    download_url: Option<String>,
    #[serde(default)]
    beta: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AltStoreVersion {
    version: String,
    #[serde(rename = "downloadURL")]
    download_url: String,
}

impl AltStoreApp {
    fn into_source_app(self, source_name: &str) -> Option<SourceApp> {
        let mut versions: Vec<AppVersion> = self
            .versions
            .into_iter()
            .map(|v| AppVersion {
                version: v.version,
                download_url: v.download_url,
                prerelease: self.beta,
            })
            .collect();
        if versions.is_empty() {
            versions.push(AppVersion {
                version: self.version.unwrap_or_else(|| "latest".to_string()),
                download_url: self.download_url?,
                prerelease: self.beta,
            });
        }
        Some(SourceApp {
            name: self.name,
            source_name: source_name.to_string(),
            bundle_id: self.bundle_identifier,
            versions,
        })
    }
}

async fn fetch_altstore_feed(url: &str) -> Result<AltStoreFeed, String> {
    let response = client()?.get(url).send().await.map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("server returned {}", response.status()));
    }
    response.json().await.map_err(|e| e.to_string())
}

/// Downloads `url` into `dest`, reporting progress from 0 to 100 when the size is known.
pub(crate) async fn download(
    url: &str,
    dest: &Path,
    mut on_progress: impl FnMut(i32),
) -> Result<(), String> {
    let mut response = client()?
        .get(url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("Download failed: {e}"))?;
    let total = response.content_length();

    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| e.to_string())?;
    }
    let mut file = tokio::fs::File::create(dest)
        .await
        .map_err(|e| e.to_string())?;

    use tokio::io::AsyncWriteExt;
    let mut received: u64 = 0;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("Download failed: {e}"))?
    {
        file.write_all(&chunk).await.map_err(|e| e.to_string())?;
        received += chunk.len() as u64;
        if let Some(total) = total.filter(|t| *t > 0) {
            on_progress((received * 100 / total) as i32);
        }
    }
    file.flush().await.map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{EXTRA_DEFAULT_SOURCES, SourcesStore, github_repo};

    #[test]
    fn extra_defaults_are_added_once_and_stay_removed() {
        let directory = std::env::temp_dir().join(format!("jimpactor-sources-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let _ = std::fs::remove_file(directory.join(super::SOURCES_FILE));

        let mut store = SourcesStore::load(&directory);
        assert!(store.sources()[0].is_default());
        for (_, url) in EXTRA_DEFAULT_SOURCES {
            assert!(store.sources().iter().any(|source| source.url == url));
        }
        store.remove(EXTRA_DEFAULT_SOURCES[0].1);
        store.save(&directory).unwrap();

        let reloaded = SourcesStore::load(&directory);
        assert!(!reloaded.sources().iter().any(|source| source.url == EXTRA_DEFAULT_SOURCES[0].1));
        assert_eq!(reloaded.sources().len(), EXTRA_DEFAULT_SOURCES.len());
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn identifies_github_repos() {
        let expected = Some(("SideStore".to_string(), "SideStore".to_string()));
        assert_eq!(
            github_repo("https://github.com/SideStore/SideStore"),
            expected
        );
        assert_eq!(
            github_repo("https://github.com/SideStore/SideStore/"),
            expected
        );
        assert_eq!(
            github_repo("https://github.com/SideStore/SideStore.git"),
            expected
        );
        assert_eq!(
            github_repo("https://github.com/SideStore/SideStore/releases"),
            expected
        );
        assert_eq!(github_repo("SideStore/SideStore"), expected);
    }

    #[test]
    fn strips_versions_from_asset_names() {
        use super::strip_version_suffix;
        assert_eq!(
            strip_version_suffix("com.kdt.livecontainer_3.3.1"),
            "com.kdt.livecontainer"
        );
        assert_eq!(strip_version_suffix("SideStore-v0.6.2"), "SideStore");
        assert_eq!(
            strip_version_suffix("LiveContainer+SideStore"),
            "LiveContainer+SideStore"
        );
        assert_eq!(strip_version_suffix("Some-App"), "Some-App");
    }

    #[test]
    fn rejects_non_repo_urls() {
        assert_eq!(
            github_repo("https://github.com/SideStore/apps/raw/main/apps.json"),
            None
        );
        assert_eq!(github_repo("https://apps.sidestore.io/"), None);
        assert_eq!(github_repo("https://github.com/SideStore"), None);
    }
}
