//! Imports anisette servers from a local Sideloadly install.
//!
//! Sideloadly caches the remote-anisette server it was assigned in its
//! `sessions.json`, e.g.:
//! ```json
//! { "<email>:<fp>": {
//!     "anisette_provider": { "url": "https://sideloadly.io/anisette/<id>", "_type": "RemoteAnisette" },
//!     "userhash": "<64 hex>",
//!     "anis": "Remote"
//! } }
//! ```
//! Reading this is purely a local-file import of the user's own data — no network
//! request and no Patreon/OAuth flow. The cached `url` already embeds a working
//! server id, and `userhash` is the `?u=` identity Sideloadly uses, so we pair them
//! to get the same provisioned identity.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Deserialize;

use plume_core::remote_servers::AnisetteServer;

#[derive(Deserialize)]
struct AnisetteProviderBlock {
    url: Option<String>,
    #[serde(rename = "_type")]
    kind: Option<String>,
}

#[derive(Deserialize)]
struct Session {
    anisette_provider: Option<AnisetteProviderBlock>,
    userhash: Option<String>,
}

/// `<base>/sideloadly/sessions.json`.
fn session_in(base: PathBuf) -> PathBuf {
    base.join("sideloadly").join("sessions.json")
}

/// Candidate `sessions.json` locations across platforms.
pub fn sessions_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();

    if let Ok(appdata) = std::env::var("APPDATA") {
        out.push(session_in(PathBuf::from(appdata))); // Windows: %APPDATA%\sideloadly
    }
    if let Ok(home) = std::env::var("HOME") {
        let home = PathBuf::from(home);
        // macOS
        out.push(session_in(home.join("Library").join("Application Support")));
        // Linux / fallback
        out.push(session_in(home.join(".config")));
        out.push(home.join(".sideloadly").join("sessions.json"));
    }
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        out.push(session_in(PathBuf::from(xdg)));
    }
    out
}

/// Parses the given `sessions.json` text into selectable anisette servers, pairing
/// each cached `url` with its `userhash` as the `?u=` identity.
pub fn parse_sessions(text: &str) -> Vec<AnisetteServer> {
    let sessions: HashMap<String, serde_json::Value> = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    let mut servers = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (key, value) in sessions {
        let Ok(session) = serde_json::from_value::<Session>(value) else {
            continue;
        };
        let Some(provider) = session.anisette_provider else {
            continue;
        };
        if provider.kind.as_deref() != Some("RemoteAnisette") {
            continue;
        }
        let Some(mut url) = provider.url else {
            continue;
        };
        url = url.trim().trim_end_matches('/').to_string();
        if url.is_empty() {
            continue;
        }
        if let Some(hash) = session.userhash.as_deref().filter(|h| !h.is_empty()) {
            if !url.contains("u=") {
                url = format!("{url}?u={hash}");
            }
        }
        if !seen.insert(url.clone()) {
            continue;
        }
        // Label with the account the session belongs to, when the key carries it.
        let label = key
            .split(':')
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or("Sideloadly");
        servers.push(AnisetteServer::new(format!("Sideloadly ({label})"), url));
    }
    servers.sort_by(|a, b| a.name.cmp(&b.name));
    servers
}

/// Loads Sideloadly's cached anisette servers from the first `sessions.json` found.
pub fn load_sideloadly_servers() -> Vec<AnisetteServer> {
    for path in sessions_paths() {
        if let Ok(text) = std::fs::read_to_string(&path) {
            let servers = parse_sessions(&text);
            if !servers.is_empty() {
                log::info!(
                    "Loaded {} Sideloadly anisette server(s) from {}",
                    servers.len(),
                    path.display()
                );
                return servers;
            }
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sideloadly_session() {
        let text = r#"{"a@b.com:dd0d": {"anisette_provider": {"url": "https://sideloadly.io/anisette/FzjicvV2PLvbwn14Bmj7s", "_type": "RemoteAnisette"}, "userhash": "5dbb18c7e55c7274ee8192b3f239542cb8c537d3131eabfa5367ee35ff937966", "anis": "Remote"}, "latest:a": "a@b.com"}"#;
        let servers = parse_sessions(text);
        assert_eq!(servers.len(), 1);
        assert_eq!(
            servers[0].address,
            "https://sideloadly.io/anisette/FzjicvV2PLvbwn14Bmj7s?u=5dbb18c7e55c7274ee8192b3f239542cb8c537d3131eabfa5367ee35ff937966"
        );
        assert!(servers[0].name.contains("a@b.com"));
    }

    #[test]
    fn ignores_non_remote_and_garbage() {
        assert!(parse_sessions("not json").is_empty());
        assert!(parse_sessions(r#"{"x": {"anis": "Local"}}"#).is_empty());
    }
}
