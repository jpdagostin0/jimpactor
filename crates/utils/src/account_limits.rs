//! What an Apple account may still do: free accounts are limited, paid ones barely.

use plume_core::developer::DeveloperSession;

use crate::{Device, Error};

/// Free accounts may create this many App IDs in any 7 days.
pub const FREE_APP_ID_LIMIT: u32 = 10;
/// iOS runs at most this many free-account apps on one device.
pub const FREE_ACTIVE_APP_LIMIT: u32 = 3;
/// Devices of one family a paid membership may register per year.
pub const PAID_DEVICE_LIMIT: u32 = 100;
const FREE_PROFILE_DAYS: u32 = 7;
const PAID_PROFILE_DAYS: u32 = 365;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountKind {
    Free,
    Paid,
}

/// How much of a quota is used. `limit` is `None` when there is no fixed limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Meter {
    pub used: u32,
    pub limit: Option<u32>,
}

impl Meter {
    pub fn is_full(&self) -> bool {
        self.limit.is_some_and(|limit| self.used >= limit)
    }
}

impl std::fmt::Display for Meter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.limit {
            Some(limit) => write!(f, "{} / {limit}", self.used),
            None => write!(f, "{}", self.used),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountLimits {
    pub kind: AccountKind,
    pub team_name: String,
    pub app_ids: Meter,
    pub registered_devices: Meter,
    pub certificates: u32,
    /// How long a provisioning profile from this account stays valid.
    pub profile_days: u32,
}

impl AccountLimits {
    /// Reads the team's membership and usage from Apple.
    pub async fn fetch(session: &DeveloperSession, team_id: &str) -> Result<Self, Error> {
        let team = session
            .qh_list_teams()
            .await?
            .teams
            .into_iter()
            .find(|team| team.team_id == team_id)
            .ok_or_else(|| Error::Other(format!("Team {team_id} is not on this account")))?;
        let kind = if team.is_free() {
            AccountKind::Free
        } else {
            AccountKind::Paid
        };

        let app_ids = session.qh_list_app_ids(team_id).await?;
        let listed = app_ids.app_ids.len() as u32;
        let quantity = |value: Option<plist::Integer>| {
            value
                .and_then(|value| value.as_unsigned())
                .and_then(|value| u32::try_from(value).ok())
        };
        let app_ids = app_id_meter(
            kind,
            listed,
            quantity(app_ids.max_quantity),
            quantity(app_ids.available_quantity),
        );

        let devices = session.qh_list_devices(team_id).await?.devices.len() as u32;
        let certificates = session.qh_list_certs(team_id).await?.certificates.len() as u32;

        Ok(Self {
            kind,
            team_name: team.name,
            app_ids,
            registered_devices: Meter {
                used: devices,
                limit: (kind == AccountKind::Paid).then_some(PAID_DEVICE_LIMIT),
            },
            certificates,
            profile_days: match kind {
                AccountKind::Free => FREE_PROFILE_DAYS,
                AccountKind::Paid => PAID_PROFILE_DAYS,
            },
        })
    }
}

/// Apple's own counts when it reports them, otherwise what is listed against the known limit.
fn app_id_meter(kind: AccountKind, listed: u32, max: Option<u32>, available: Option<u32>) -> Meter {
    match (max, available) {
        (Some(max), Some(available)) => Meter {
            used: max.saturating_sub(available),
            limit: Some(max),
        },
        _ => Meter {
            used: listed,
            limit: (kind == AccountKind::Free).then_some(FREE_APP_ID_LIMIT),
        },
    }
}

impl Device {
    /// Sideloaded apps on this device whose profile is still valid. On a free account these
    /// count towards [`FREE_ACTIVE_APP_LIMIT`].
    pub async fn active_sideloaded_apps(&self) -> Result<u32, Error> {
        let now = std::time::SystemTime::now();
        Ok(self
            .installed_apps()
            .await?
            .iter()
            .filter(|app| app.sideloaded)
            .filter(|app| {
                app.expiration
                    .is_none_or(|date| std::time::SystemTime::from(date) > now)
            })
            .count() as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apples_counts_win_over_the_listed_apps() {
        let meter = app_id_meter(AccountKind::Free, 2, Some(10), Some(3));
        assert_eq!(meter.used, 7);
        assert_eq!(meter.limit, Some(10));
        assert!(!meter.is_full());
    }

    #[test]
    fn free_accounts_fall_back_to_the_known_limit_and_paid_ones_have_none() {
        let free = app_id_meter(AccountKind::Free, 10, None, None);
        assert!(free.is_full());
        assert_eq!(free.to_string(), "10 / 10");
        let paid = app_id_meter(AccountKind::Paid, 40, None, None);
        assert_eq!(paid.limit, None);
        assert!(!paid.is_full());
        assert_eq!(paid.to_string(), "40");
    }
}
