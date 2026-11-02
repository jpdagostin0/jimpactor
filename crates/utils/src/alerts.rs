//! Which installed apps are close enough to expiring to tell the user about.

use std::time::{Duration, SystemTime};

use crate::SignerAppReal;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpiringApp {
    pub bundle_id: String,
    pub name: String,
    pub expiration: SystemTime,
}

/// Sideloaded apps that have expired or will within `window`, soonest first.
pub fn expiring_apps(
    apps: &[SignerAppReal],
    now: SystemTime,
    window: Duration,
) -> Vec<ExpiringApp> {
    let mut expiring: Vec<_> = apps
        .iter()
        .filter(|app| app.sideloaded)
        .filter_map(|app| {
            let expiration = SystemTime::from(app.expiration?);
            let bundle_id = app.bundle_id.clone()?;
            (expiration <= now + window).then(|| ExpiringApp {
                bundle_id,
                name: app
                    .installed_name
                    .clone()
                    .unwrap_or_else(|| app.app.to_string()),
                expiration,
            })
        })
        .collect();
    expiring.sort_by_key(|app| app.expiration);
    expiring
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: Duration = Duration::from_secs(24 * 60 * 60);

    fn app(id: &str, expires: Option<SystemTime>, sideloaded: bool) -> SignerAppReal {
        SignerAppReal {
            expiration: expires.map(plist::Date::from),
            sideloaded,
            ..SignerAppReal::from_bundle_identifier(Some(id))
        }
    }

    #[test]
    fn only_sideloaded_apps_inside_the_window_alert_soonest_first() {
        let now = SystemTime::now();
        let apps = [
            app("later", Some(now + DAY * 5), true),
            app("soon", Some(now + DAY), true),
            app("expired", Some(now - DAY), true),
            app("store", Some(now + DAY), false),
            app("unknown", None, true),
        ];
        let ids: Vec<_> = expiring_apps(&apps, now, DAY * 2)
            .into_iter()
            .map(|app| app.bundle_id)
            .collect();
        assert_eq!(ids, ["expired", "soon"]);
    }
}
