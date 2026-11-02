//! Opening a developer session for a stored account, shared by every front end.

use std::path::Path;

use plume_core::{CertificateIdentity, developer::DeveloperSession};
use plume_store::{AnisetteSettings, GsaAccount};

use crate::refresh::ConfirmReset;

/// Which team to act on. A stored team must still exist; with none stored, an account with a
/// single team uses it.
pub fn resolve_team(stored: &str, available: &[&str]) -> Result<String, String> {
    if available.is_empty() {
        return Err("No teams available for this account".into());
    }
    if !stored.is_empty() {
        return available
            .contains(&stored)
            .then(|| stored.to_owned())
            .ok_or_else(|| {
                format!(
                    "Stored team ID '{stored}' not found in available teams. Please update your team selection in Settings."
                )
            });
    }
    match available {
        [only] => Ok((*only).to_owned()),
        _ => Err("Choose a developer team in Settings > Accounts first.".into()),
    }
}

/// Opens a session for `account` and resolves its team.
pub async fn open_session(
    account: &GsaAccount,
    data_path: &Path,
) -> Result<(DeveloperSession, String), String> {
    let session = DeveloperSession::new(
        account.adsid().clone(),
        account.xcode_gs_token().clone(),
        AnisetteSettings::load_configuration(data_path),
    )
    .await
    .map_err(|e| e.to_string())?;
    let teams = session
        .qh_list_teams()
        .await
        .map_err(|e| e.to_string())?
        .teams;
    let ids: Vec<&str> = teams.iter().map(|team| team.team_id.as_str()).collect();
    let team_id = resolve_team(account.team_id(), &ids)?;
    Ok((session, team_id))
}

/// The account's teams as (name, ID).
pub async fn list_teams(
    account: &GsaAccount,
    data_path: &Path,
) -> Result<Vec<(String, String)>, String> {
    let session = DeveloperSession::new(
        account.adsid().clone(),
        account.xcode_gs_token().clone(),
        AnisetteSettings::load_configuration(data_path),
    )
    .await
    .map_err(|e| e.to_string())?;
    Ok(session
        .qh_list_teams()
        .await
        .map_err(|e| e.to_string())?
        .teams
        .into_iter()
        .map(|team| (team.name, team.team_id))
        .collect())
}

/// The account's signing certificate as p12 data, with the team it belongs to.
pub async fn export_certificate(
    account: &GsaAccount,
    data_path: &Path,
    confirm: ConfirmReset,
) -> Result<(String, Vec<u8>), String> {
    let (session, team_id) = open_session(account, data_path).await?;
    let mut on_certificate_reset = move || confirm();
    let identity = CertificateIdentity::new_with_session(
        &session,
        data_path.to_path_buf(),
        None,
        &team_id,
        true,
        Some(&mut on_certificate_reset),
    )
    .await
    .map_err(|e| e.to_string())?;
    let data = identity.p12_data.ok_or("Missing p12 data")?;
    Ok((team_id, data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn teams_resolve_without_guessing() {
        assert_eq!(resolve_team("", &["ONE"]).unwrap(), "ONE");
        assert_eq!(resolve_team("TWO", &["ONE", "TWO"]).unwrap(), "TWO");
        assert!(resolve_team("", &["ONE", "TWO"]).is_err());
        assert!(resolve_team("GONE", &["ONE"]).is_err());
        assert!(resolve_team("", &[]).is_err());
    }
}
