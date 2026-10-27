//! Public anisette v3 server list, as maintained by SideStore.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::AnisetteError;

pub const SERVER_LIST_URL: &str = "https://servers.sidestore.io/servers.json";

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AnisetteServer {
    pub name: String,
    pub address: String,
}

impl AnisetteServer {
    pub fn new(name: impl Into<String>, address: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            address: address.into().trim().trim_end_matches('/').to_string(),
        }
    }
}

impl std::fmt::Display for AnisetteServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.name, self.address)
    }
}

/// Used when the server list can't be fetched.
pub fn builtin_servers() -> Vec<AnisetteServer> {
    vec![
        AnisetteServer::new("jkcoxson", crate::DEFAULT_ANISETTE_URL_V3),
        AnisetteServer::new("SideStore", "https://ani.sidestore.io"),
        AnisetteServer::new("SideStore (.app)", "https://ani.sidestore.app"),
        AnisetteServer::new("SideStore (.zip)", "https://ani.sidestore.zip"),
    ]
}

pub async fn fetch_server_list() -> Result<Vec<AnisetteServer>, AnisetteError> {
    #[derive(Deserialize)]
    struct ServerList {
        servers: Vec<AnisetteServer>,
    }

    let list = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?
        .get(SERVER_LIST_URL)
        .send()
        .await?
        .error_for_status()?
        .json::<ServerList>()
        .await?;

    Ok(list
        .servers
        .into_iter()
        .map(|s| AnisetteServer::new(s.name, s.address))
        .collect())
}

/// Checks every server (from the live list, falling back to the builtin one) and returns
/// them sorted fastest first, keeping only those that responded.
pub async fn rank_servers() -> Vec<(AnisetteServer, Duration)> {
    let servers = match fetch_server_list().await {
        Ok(servers) if !servers.is_empty() => servers,
        _ => builtin_servers(),
    };

    let checks = servers.into_iter().map(|server| async move {
        check_server(&server.address)
            .await
            .ok()
            .map(|rtt| (server, rtt))
    });
    let mut ranked: Vec<(AnisetteServer, Duration)> = futures_util::future::join_all(checks)
        .await
        .into_iter()
        .flatten()
        .collect();
    ranked.sort_by_key(|(_, rtt)| *rtt);
    ranked
}

/// Checks that `address` speaks the anisette v3 protocol, returning the round-trip time.
pub async fn check_server(address: &str) -> Result<Duration, AnisetteError> {
    #[derive(Deserialize)]
    struct ClientInfo {
        #[allow(dead_code)]
        client_info: String,
    }

    let start = Instant::now();
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?
        .get(format!(
            "{}/v3/client_info",
            address.trim().trim_end_matches('/')
        ))
        .send()
        .await?
        .error_for_status()?
        .json::<ClientInfo>()
        .await?;
    Ok(start.elapsed())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "network"]
    async fn fetch_list_and_check_first_server() -> Result<(), AnisetteError> {
        let servers = fetch_server_list().await?;
        assert!(!servers.is_empty());
        check_server(&servers[0].address).await?;
        Ok(())
    }
}

#[cfg(test)]
mod rank_tests {
    use super::*;

    #[tokio::test]
    #[ignore = "network"]
    async fn rank_picks_a_working_server() {
        let ranked = rank_servers().await;
        assert!(!ranked.is_empty(), "no servers responded");
        for (s, rtt) in ranked.iter().take(5) {
            println!("{:>6}ms  {}", rtt.as_millis(), s);
        }
    }
}
