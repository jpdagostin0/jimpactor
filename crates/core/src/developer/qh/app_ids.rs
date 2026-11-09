use plist::{Dictionary, Integer, Value};
use serde::Deserialize;

use crate::Error;

use super::{DeveloperSession, QHResponseMeta};
use crate::developer::strip_invalid_chars;
use crate::developer_endpoint;

impl DeveloperSession {
    pub async fn qh_list_app_ids(&self, team_id: &str) -> Result<AppIDsResponse, Error> {
        let endpoint = developer_endpoint!("/QH65B2/ios/listAppIds.action");

        let mut body = Dictionary::new();
        body.insert("teamId".to_string(), Value::String(team_id.to_owned()));

        let response = self.qh_send_request(&endpoint, Some(body)).await?;
        let response_data: AppIDsResponse = plist::from_value(&Value::Dictionary(response))?;

        Ok(response_data)
    }

    pub async fn qh_add_app_id(
        &self,
        team_id: &str,
        name: &str,
        identifier: &str,
    ) -> Result<AppIDResponse, Error> {
        let endpoint = developer_endpoint!("/QH65B2/ios/addAppId.action");

        let mut body = Dictionary::new();
        body.insert("teamId".to_string(), Value::String(team_id.to_owned()));
        body.insert("name".to_string(), Value::String(strip_invalid_chars(name)));
        body.insert(
            "identifier".to_string(),
            Value::String(identifier.to_owned()),
        );

        let response = self.qh_send_request(&endpoint, Some(body)).await?;
        let response_data: AppIDResponse = plist::from_value(&Value::Dictionary(response))?;

        Ok(response_data)
    }

    pub async fn qh_delete_app_id(
        &self,
        team_id: &str,
        app_id_id: &str,
    ) -> Result<QHResponseMeta, Error> {
        let endpoint = developer_endpoint!("/QH65B2/ios/deleteAppId.action");

        let mut body = Dictionary::new();
        body.insert("teamId".to_string(), Value::String(team_id.to_owned()));
        body.insert("appIdId".to_string(), Value::String(app_id_id.to_owned()));

        let response = self.qh_send_request(&endpoint, Some(body)).await?;
        let response_data: QHResponseMeta = plist::from_value(&Value::Dictionary(response))?;

        Ok(response_data)
    }

    pub async fn qh_update_app_id(
        &self,
        team_id: &str,
        app_id_id: &str,
        features: Dictionary,
    ) -> Result<AppIDResponse, Error> {
        let endpoint = developer_endpoint!("/QH65B2/ios/updateAppId.action");

        let mut body = Dictionary::new();
        body.insert("teamId".to_string(), Value::String(team_id.to_owned()));
        body.insert("appIdId".to_string(), Value::String(app_id_id.to_owned()));

        for (key, value) in features {
            body.insert(key, value);
        }

        let response = self.qh_send_request(&endpoint, Some(body)).await?;
        let response_data: AppIDResponse = plist::from_value(&Value::Dictionary(response))?;

        Ok(response_data)
    }

    pub async fn qh_get_app_id(
        &self,
        team_id: &str,
        identifier: &String,
    ) -> Result<Option<AppID>, Error> {
        let response_data = self.qh_list_app_ids(team_id).await?;

        let app_id = response_data
            .app_ids
            .into_iter()
            .find(|app| app.identifier == *identifier);

        Ok(app_id)
    }

    pub async fn qh_ensure_app_id(
        &self,
        team_id: &str,
        name: &str,
        identifier: &String,
    ) -> Result<AppID, Error> {
        if let Some(app_id) = self.qh_get_app_id(team_id, identifier).await? {
            Ok(app_id)
        } else {
            let response = self.qh_add_app_id(team_id, name, identifier).await?;
            Ok(response.app_id)
        }
    }
}

#[allow(dead_code)]
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AppIDsResponse {
    pub app_ids: Vec<AppID>,
    /// App IDs the team may hold, when Apple reports it.
    #[serde(default, deserialize_with = "some_integer")]
    pub max_quantity: Option<Integer>,
    /// App IDs the team can still create, when Apple reports it.
    #[serde(default, deserialize_with = "some_integer")]
    pub available_quantity: Option<Integer>,
    #[serde(flatten)]
    pub meta: QHResponseMeta,
}

/// Reads a present integer as `Some`. The `flatten` above makes serde read this struct
/// as a map, where plist expects options in its explicit `{"Some": ..}` form and fails
/// on a bare integer ("expected StartDictionary, found Integer"). Absent keys fall back
/// to `None` through `#[serde(default)]`.
fn some_integer<'de, D>(deserializer: D) -> Result<Option<Integer>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Integer::deserialize(deserializer).map(Some)
}

#[allow(dead_code)]
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AppIDResponse {
    pub app_id: AppID,
    #[serde(flatten)]
    pub meta: QHResponseMeta,
}

#[allow(dead_code)]
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AppID {
    pub app_id_id: String,
    pub name: String,
    app_id_platform: String,
    prefix: String,
    pub identifier: String,
    is_wild_card: bool,
    is_duplicate: bool,
    features: Features,
    enabled_features: Option<Vec<String>>,
    is_dev_push_enabled: bool,
    is_prod_push_enabled: bool,
    associated_application_groups_count: Option<Integer>,
    associated_cloud_containers_count: Option<Integer>,
    associated_identifiers_count: Option<Integer>,
}

#[allow(dead_code)]
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct Features {
    push: bool,
    i_cloud: bool, // com.apple.developer.icloud-container-development-container-identifiers, com.apple.developer.icloud-services, com.apple.developer.icloud-container-environment, com.apple.developer.ubiquity-kvstore-identifier, com.apple.developer.ubiquity-container-identifiers, com.apple.developer.icloud-container-identifiers
    in_app_purchase: bool,
    game_center: bool, // com.apple.developer.game-center // bro this doesnt turn off if this is on at all
    passbook: bool,    // com.apple.developer.pass-type-identifiers
    // IAD53UNK2F inter-app-audio
    // V66P55NK2I com.apple.developer.networking.vpn.api
    data_protection: String, // com.apple.developer.default-data-protection // complete, unlessopen, untilfirstauth
    // SKC3T5S89Y com.apple.developer.associated-domains
    // APG3427HIY com.apple.security.application-groups
    // HK421J6T7P com.apple.developer.healthkit, com.apple.developer.healthkit.access, com.apple.developer.healthkit.background-delivery
    home_kit: bool, // com.apple.developer.homekit
    // WC421J6T7P com.apple.external-accessory.wireless-configuration
    // OM633U5T5G com.apple.developer.in-app-payments
    cloud_kit_version: Integer,
    // SI015DKUHP com.apple.developer.siri
    // NWEXT04537 com.apple.developer.networking.networkextension
    // HSC639VEI8 com.apple.developer.networking.HotspotConfiguration
    // MP49FN762P com.apple.developer.networking.multipath
    // NFCTRMAY17 com.apple.developer.nfc.readersession.formats
    // PKTJAN2017 com.apple.developer.ClassKit-environment
    // CPEQ28MX4E com.apple.developer.authentication-services.autofill-credential-provider
    // USER_MANAGEMENT com.apple.developer.user-management
    // FONT_INSTALLATION com.apple.developer.user-fonts
    // APPLE_ID_AUTH com.apple.developer.applesignin
    // NETWORK_CUSTOM_PROTOCOL com.apple.developer.networking.custom-protocol
    // SYSTEM_EXTENSION_INSTALL com.apple.developer.system-extension.install
    // AWEQ28MY3E com.apple.developer.networking.wifi-info
}

impl Features {
    // pub fn get_feature_for_entitlement(entitlement: &str) -> Option<&'static str> {

    // }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(quantities: bool) -> Value {
        let mut features = Dictionary::new();
        for key in [
            "push",
            "iCloud",
            "inAppPurchase",
            "gameCenter",
            "passbook",
            "homeKit",
        ] {
            features.insert(key.into(), Value::Boolean(false));
        }
        features.insert("dataProtection".into(), Value::String(String::new()));
        features.insert("cloudKitVersion".into(), Value::Integer(2.into()));

        let mut app_id = Dictionary::new();
        for (key, value) in [
            ("appIdId", "VGCBDPKG3N"),
            ("name", "LiveContainer"),
            ("appIdPlatform", "ios"),
            ("prefix", "3A87L6ZM3L"),
            ("identifier", "com.kdt.livecontainer.3A87L6ZM3L"),
        ] {
            app_id.insert(key.into(), Value::String(value.into()));
        }
        for key in [
            "isWildCard",
            "isDuplicate",
            "isDevPushEnabled",
            "isProdPushEnabled",
        ] {
            app_id.insert(key.into(), Value::Boolean(false));
        }
        app_id.insert("features".into(), Value::Dictionary(features));
        app_id.insert(
            "associatedApplicationGroupsCount".into(),
            Value::Integer(3.into()),
        );

        let mut response = Dictionary::new();
        response.insert(
            "appIds".into(),
            Value::Array(vec![Value::Dictionary(app_id)]),
        );
        if quantities {
            response.insert("maxQuantity".into(), Value::Integer(10.into()));
            response.insert("availableQuantity".into(), Value::Integer(7.into()));
        }
        for (key, value) in [
            ("creationTimestamp", "2026-11-09T15:37:17Z"),
            ("userLocale", "en_US"),
            ("protocolVersion", "QH65B2"),
            ("responseId", "53ecb8cc"),
        ] {
            response.insert(key.into(), Value::String(value.into()));
        }
        response.insert("resultCode".into(), Value::Integer(0.into()));
        response.insert("pageNumber".into(), Value::Integer(0.into()));
        Value::Dictionary(response)
    }

    #[test]
    fn reads_app_id_quantities_when_apple_reports_them() {
        let parsed: AppIDsResponse = plist::from_value(&response(true)).unwrap();
        assert_eq!(parsed.app_ids.len(), 1);
        assert_eq!(parsed.max_quantity, Some(10.into()));
        assert_eq!(parsed.available_quantity, Some(7.into()));
        assert_eq!(parsed.meta.page_number, Some(0.into()));
    }

    #[test]
    fn app_id_quantities_are_optional() {
        let parsed: AppIDsResponse = plist::from_value(&response(false)).unwrap();
        assert_eq!(parsed.max_quantity, None);
        assert_eq!(parsed.available_quantity, None);
    }
}
