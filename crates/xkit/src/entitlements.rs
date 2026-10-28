//! Typed entitlement access while preserving keys unknown to XKit.
use crate::{Result, invalid};
use plist::{Dictionary, Value};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeveloperServicesCapability {
    pub capability_type: String,
    pub is_free: bool,
    pub settings: Vec<Json>,
}

macro_rules! entitlement_types {
    ($($variant:ident($ty:ty) => $key:literal),* $(,)?) => {
        #[derive(Clone, Debug, PartialEq, Eq)]
        pub enum Entitlement { $($variant($ty)),* }
        impl Entitlement {
            pub fn identifier(&self) -> &'static str {
                match self { $(Self::$variant(_) => $key),* }
            }
            pub fn value(&self) -> Result<Value> {
                match self { $(Self::$variant(value) => Ok(plist::to_value(value)?)),* }
            }
            pub fn parse(key: &str, value: &Value) -> Result<Option<Self>> {
                Ok(match key { $($key => Some(Self::$variant(plist::from_value(value)?))),*, _ => None })
            }
        }
    };
}

entitlement_types! {
    ApplicationIdentifier(String) => "application-identifier",
    TeamIdentifier(String) => "com.apple.developer.team-identifier",
    KeychainAccessGroups(Vec<String>) => "keychain-access-groups",
    GetTaskAllow(bool) => "get-task-allow",
    AssociatedDomains(Vec<String>) => "com.apple.developer.associated-domains",
    APSEnvironment(APSEnvironment) => "aps-environment",
    AppGroups(Vec<String>) => "com.apple.security.application-groups",
    DataProtection(DataProtection) => "com.apple.developer.default-data-protection",
    NetworkExtension(bool) => "com.apple.developer.networking.networkextension",
    Multipath(bool) => "com.apple.developer.networking.multipath",
    VPNConfiguration(bool) => "com.apple.networking.vpn.configuration",
    SiriKit(bool) => "com.apple.developer.siri",
    InterAppAudio(bool) => "inter-app-audio",
    WirelessAccessory(bool) => "com.apple.external-accessory.wireless-configuration",
    WiFiAware(Vec<String>) => "com.apple.developer.wifi-aware",
    HomeKit(bool) => "com.apple.developer.homekit",
    HealthKit(bool) => "com.apple.developer.healthkit",
    AppleSignIn(Vec<String>) => "com.apple.developer.applesignin",
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum APSEnvironment {
    Development,
    Production,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DataProtection {
    #[serde(rename = "NSFileProtectionComplete")]
    Complete,
    #[serde(rename = "NSFileProtectionCompleteUnlessOpen")]
    UnlessOpen,
    #[serde(rename = "NSFileProtectionCompleteUntilFirstUserAuthentication")]
    UntilFirstAuth,
}

impl Entitlement {
    pub fn capability(&self) -> Option<DeveloperServicesCapability> {
        use Entitlement::*;
        let (name, is_free) = match self {
            AppGroups(_) => ("APP_GROUPS", false),
            AssociatedDomains(_) => ("ASSOCIATED_DOMAINS", false),
            HealthKit(_) => ("HEALTHKIT", true),
            HomeKit(_) => ("HOMEKIT", true),
            InterAppAudio(_) => ("INTER_APP_AUDIO", true),
            Multipath(_) => ("MULTIPATH", false),
            NetworkExtension(_) => ("NETWORK_EXTENSIONS", false),
            VPNConfiguration(_) => ("PERSONAL_VPN", false),
            APSEnvironment(_) => ("PUSH_NOTIFICATIONS", false),
            SiriKit(_) => ("SIRIKIT", false),
            WirelessAccessory(_) => ("WIRELESS_ACCESSORY_CONFIGURATION", true),
            WiFiAware(_) => ("WIFI_AWARE", false),
            AppleSignIn(_) => ("APPLE_ID_AUTH", false),
            DataProtection(_) => ("DATA_PROTECTION", true),
            _ => return None,
        };
        let settings = match self {
            AppleSignIn(_) => vec![
                json!({"key":"APPLE_ID_AUTH_APP_CONSENT", "options":[{"key":"PRIMARY_APP_CONSENT"}]}),
            ],
            DataProtection(value) => {
                let option = match value {
                    crate::entitlements::DataProtection::Complete => "COMPLETE_PROTECTION",
                    crate::entitlements::DataProtection::UnlessOpen => "PROTECTED_UNLESS_OPEN",
                    crate::entitlements::DataProtection::UntilFirstAuth => {
                        "PROTECTED_UNTIL_FIRST_USER_AUTH"
                    }
                };
                vec![json!({"key":"DATA_PROTECTION_PERMISSION_LEVEL", "options":[{"key":option}]})]
            }
            _ => Vec::new(),
        };
        Some(DeveloperServicesCapability {
            capability_type: name.into(),
            is_free,
            settings,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Entitlements(pub Dictionary);

impl Entitlements {
    pub fn new(entitlements: impl IntoIterator<Item = Entitlement>) -> Result<Self> {
        let mut result = Self::default();
        for entitlement in entitlements {
            result.insert(entitlement)?;
        }
        Ok(result)
    }
    pub fn from_bytes(data: &[u8]) -> Result<Self> {
        Ok(plist::from_bytes(data)?)
    }
    pub fn to_xml(&self) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        plist::to_writer_xml(&mut bytes, self)?;
        Ok(bytes)
    }
    pub fn entitlements(&self) -> Result<Vec<Entitlement>> {
        self.0
            .iter()
            .filter_map(|(k, v)| Entitlement::parse(k, v).transpose())
            .collect()
    }
    pub fn insert(&mut self, entitlement: Entitlement) -> Result<()> {
        self.0
            .insert(entitlement.identifier().into(), entitlement.value()?);
        Ok(())
    }
    /// Replaces recognized entitlements; unrecognized plist keys survive.
    pub fn set_entitlements(&mut self, values: Vec<Entitlement>) -> Result<()> {
        let old = self.entitlements()?;
        for value in old {
            self.0.remove(value.identifier());
        }
        for value in values {
            self.insert(value)?;
        }
        Ok(())
    }
    pub fn update(&mut self, team_id: &str, bundle_id: &str) -> Result<()> {
        if team_id.is_empty() || bundle_id.is_empty() {
            return Err(invalid("Empty team or bundle ID"));
        }
        let application_id = format!("{team_id}.{bundle_id}");
        self.insert(Entitlement::ApplicationIdentifier(application_id.clone()))?;
        self.insert(Entitlement::TeamIdentifier(team_id.into()))?;
        self.insert(Entitlement::KeychainAccessGroups(vec![application_id]))?;
        Ok(())
    }
    /// Free teams receive only recognized entitlements with free capabilities.
    pub fn for_free_team(&self) -> Result<Self> {
        Self::new(
            self.entitlements()?
                .into_iter()
                .filter(|e| e.capability().is_none_or(|c| c.is_free)),
        )
    }
    pub fn capabilities(&self) -> Result<Vec<DeveloperServicesCapability>> {
        Ok(self
            .entitlements()?
            .into_iter()
            .filter_map(|e| e.capability())
            .collect())
    }
}
