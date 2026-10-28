use super::{
    DeveloperAPIAuthData, DeveloperAPIClient, DeveloperServicesCertificate,
    DeveloperServicesPlatform, IdentifierPolicy, Resource,
};
use crate::{
    Error, Result,
    entitlements::{Entitlement, Entitlements},
    invalid,
    model::{Certificate, Keypair, Mobileprovision, SigningInfo},
    signer::{Signer, SigningContext, TargetDevice},
    utilities::{ProvisioningIdentifiers as IDs, SigningInfoManager},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures::future::BoxFuture;
use reqwest::Method;
use serde_json::{Value as Json, json};
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
};

pub type ConfirmRevocation =
    dyn Fn(Vec<DeveloperServicesCertificate>) -> BoxFuture<'static, bool> + Send + Sync;

#[derive(Clone, Debug)]
pub struct ProvisioningInfo {
    pub new_bundle_id: String,
    pub entitlements: Entitlements,
    pub mobileprovision: Mobileprovision,
}
pub struct ProvisioningResponse {
    pub signing_info: SigningInfo,
    pub provisioning: BTreeMap<PathBuf, ProvisioningInfo>,
}

fn resource_id(value: &str, resource_type: &str) -> Json {
    json!({"id":value, "type":resource_type})
}
fn decode_content(resource: &Resource, field: &str) -> Result<Vec<u8>> {
    STANDARD
        .decode(
            resource
                .attribute(field)
                .ok_or_else(|| invalid(format!("Missing {field}")))?,
        )
        .map_err(|e| invalid(e.to_string()))
}
fn resource_path(collection: &str, id: &str) -> Result<String> {
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(invalid("Invalid developer resource ID"));
    }
    Ok(format!("v1/{collection}/{id}"))
}

impl DeveloperAPIClient {
    pub async fn registered_devices(&self) -> Result<Vec<Resource>> {
        self.collection("v1/devices", &[]).await
    }

    /// Apple's supported operation is enable/disable, not permanent deletion.
    pub async fn set_device_enabled(&self, id: &str, enabled: bool) -> Result<Resource> {
        let path = resource_path("devices", id)?;
        let response = self
            .request(
                Method::PATCH,
                &path,
                &[],
                Some(json!({"data": {
                    "type": "devices", "id": id,
                    "attributes": { "status": if enabled { "ENABLED" } else { "DISABLED" } }
                }})),
            )
            .await?;
        let device: Resource = serde_json::from_value(
            response
                .get("data")
                .cloned()
                .ok_or_else(|| invalid("Missing updated device"))?,
        )?;
        if device.id != id
            || device.attribute("status") != Some(if enabled { "ENABLED" } else { "DISABLED" })
        {
            return Err(invalid("Apple did not confirm the requested device status"));
        }
        Ok(device)
    }

    pub async fn register_device(&self, target: Option<&TargetDevice>) -> Result<()> {
        let Some(target) = target else {
            return Ok(());
        };
        // Compare by UDID before creating; avoid hiding unrelated 409 errors.
        let devices = self
            .collection(
                "v1/devices",
                &[("filter[udid]".into(), target.udid.clone())],
            )
            .await?;
        if devices.iter().any(|d| {
            d.attribute("udid")
                .is_some_and(|v| v.eq_ignore_ascii_case(&target.udid))
        }) {
            return Ok(());
        }
        self.create(
            "v1/devices",
            json!({"type":"devices", "attributes":{
            "name":target.name, "platform":"IOS", "udid":target.udid }}),
        )
        .await?;
        Ok(())
    }

    pub async fn fetch_certificate(
        &self,
        manager: &dyn SigningInfoManager,
        confirm: &ConfirmRevocation,
    ) -> Result<SigningInfo> {
        let certificates = self.collection("v1/certificates", &[]).await?;
        let saved = manager.info(self.auth.identity_id())?;
        let mut matching = None;
        if let Some(saved) = saved.as_ref() {
            matching = certificates.iter().find(|c| {
                c.attribute("serialNumber")
                    .is_some_and(|v| saved.certificate.has_serial_number(v))
            });
            if let Some(certificate) = matching {
                let expiration = certificate
                    .attribute("expirationDate")
                    .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
                    .map(|v| v.with_timezone(&chrono::Utc))
                    .unwrap_or_else(|| saved.certificate.raw().validity_not_after());
                if expiration > chrono::Utc::now() {
                    return Ok(saved.clone());
                }
            }
        }
        let is_free = self.team().await?.is_some_and(|t| t.is_free());
        if is_free {
            let to_revoke: Vec<_> = if let Some(matching) = matching {
                vec![matching.clone()]
            } else {
                certificates
                    .iter()
                    .filter(|c| {
                        matches!(
                            c.attribute("certificateType"),
                            Some("DEVELOPMENT" | "IOS_DEVELOPMENT")
                        )
                    })
                    .cloned()
                    .collect()
            };
            if matching.is_none() && !to_revoke.is_empty() && !confirm(to_revoke.clone()).await {
                return Err(Error::Cancelled);
            }
            for certificate in to_revoke {
                self.delete(&resource_path("certificates", &certificate.id)?)
                    .await?;
            }
        }
        let keypair = Keypair::new()?;
        let csr = keypair.generate_csr()?;
        let resource = self
            .create(
                "v1/certificates",
                json!({"type":"certificates", "attributes":{
            "csrContent":csr.pem_string, "certificateType":"DEVELOPMENT" }}),
            )
            .await?;
        let info = SigningInfo {
            private_key: keypair.private_key()?,
            certificate: Certificate::from_der(decode_content(&resource, "certificateContent")?)?,
        };
        manager.set_info(self.auth.identity_id(), Some(&info))?;
        Ok(info)
    }

    pub async fn assign_app_groups(
        &self,
        group_ids: &[String],
        app_id: &str,
    ) -> Result<Vec<String>> {
        let DeveloperAPIAuthData::Xcode(auth) = &self.auth else {
            return Ok(group_ids.to_vec());
        };
        let legacy = self.legacy()?;
        let platform = DeveloperServicesPlatform::IOS;
        let existing = legacy.list_app_groups(&platform, &auth.team_id).await?;
        let verbatim = self.identifier_policy == IdentifierPolicy::Verbatim;
        let key = |group: &str| {
            if verbatim {
                group.to_owned()
            } else {
                IDs::sanitize_group(group)
            }
        };
        let mut existing: HashMap<_, _> = existing
            .into_iter()
            .map(|g| (key(&g.group_id), g))
            .collect();
        let mut result = Vec::new();
        for group_id in group_ids {
            let (lookup, new_id, new_name) = if verbatim {
                let suffix = format!(".{}", auth.team_id);
                let id = if group_id.ends_with(&suffix) {
                    group_id.clone()
                } else {
                    format!("{group_id}{suffix}")
                };
                (id.clone(), id.clone(), id)
            } else {
                let sanitized = IDs::sanitize_group(group_id);
                (
                    sanitized.clone(),
                    IDs::group_id(&sanitized, self.auth.identity_id()),
                    IDs::group_name(&sanitized),
                )
            };
            let group = if let Some(group) = existing.get(&lookup) {
                group.clone()
            } else {
                let group = legacy
                    .add_app_group(&platform, &auth.team_id, &new_name, &new_id)
                    .await?;
                existing.insert(lookup, group.clone());
                group
            };
            legacy
                .assign_app_group(&platform, &auth.team_id, app_id, &group.id)
                .await?;
            result.push(group.group_id);
        }
        Ok(result)
    }

    pub async fn upsert_app(
        &self,
        bundle_id: &str,
        entitlements: &Entitlements,
    ) -> Result<Resource> {
        let sanitized = IDs::sanitize(bundle_id);
        let (new_bundle_id, app_name) = match self.identifier_policy {
            IdentifierPolicy::Xtool => (
                IDs::identifier(&sanitized, self.auth.identity_id()),
                IDs::app_name(&sanitized),
            ),
            IdentifierPolicy::Verbatim => (bundle_id.to_owned(), bundle_id.replace('.', " ")),
        };
        let existing = self
            .collection(
                "v1/bundleIds",
                &[("filter[identifier]".into(), new_bundle_id.clone())],
            )
            .await?;
        let app = if let Some(app) = existing
            .into_iter()
            .find(|a| a.attribute("identifier") == Some(new_bundle_id.as_str()))
        {
            app
        } else {
            self.create(
                "v1/bundleIds",
                json!({"type":"bundleIds", "attributes":{
                "name":app_name, "platform":"IOS", "identifier":new_bundle_id }}),
            )
            .await?
        };
        let path = format!(
            "{}/bundleIdCapabilities",
            resource_path("bundleIds", &app.id)?
        );
        let existing = self.collection(&path, &[]).await?;
        let mut wanted: HashMap<_, _> = entitlements
            .capabilities()?
            .into_iter()
            .map(|c| (c.capability_type.clone(), c))
            .collect();
        for capability in existing {
            let Some(typ) = capability.attribute("capabilityType") else {
                continue;
            };
            if let Some(wanted) = wanted.remove(typ) {
                let settings = capability
                    .attributes
                    .get("settings")
                    .and_then(Json::as_array)
                    .cloned()
                    .unwrap_or_default();
                if settings != wanted.settings {
                    self.request(
                        Method::PATCH,
                        &resource_path("bundleIdCapabilities", &capability.id)?,
                        &[],
                        Some(
                            json!({"data":{"id":capability.id, "type":"bundleIdCapabilities",
                            "attributes":{"capabilityType":typ,"settings":wanted.settings}}}),
                        ),
                    )
                    .await?;
                }
            } else if typ != "IN_APP_PURCHASE" {
                self.delete(&resource_path("bundleIdCapabilities", &capability.id)?)
                    .await?;
            }
        }
        for (typ, capability) in wanted {
            let mut relationships = json!({"bundleId":{"data":resource_id(&app.id,"bundleIds")}});
            if !self.auth.is_public_api() {
                relationships["capability"] = json!({"data":resource_id(&typ,"capabilities")});
            }
            self.create("v1/bundleIdCapabilities", json!({"type":"bundleIdCapabilities", "attributes":{
                "capabilityType":typ, "settings":capability.settings }, "relationships":relationships})).await?;
        }
        Ok(app)
    }

    pub async fn fetch_profile(
        &self,
        bundle_id: &str,
        signing_info: &SigningInfo,
    ) -> Result<Mobileprovision> {
        let matching: Vec<_> = self
            .collection(
                "v1/bundleIds",
                &[
                    ("filter[identifier]".into(), bundle_id.into()),
                    ("include".into(), "profiles".into()),
                    ("fields[profiles]".into(), "bundleId".into()),
                ],
            )
            .await?
            .into_iter()
            .filter(|b| b.attribute("identifier") == Some(bundle_id))
            .collect();
        if matching.len() != 1 {
            return Err(invalid(format!(
                "Expected one bundle ID '{bundle_id}', found {}",
                matching.len()
            )));
        }
        let bundle = &matching[0];
        if let Some(profiles) = bundle
            .relationships
            .get("profiles")
            .and_then(|p| p.get("data"))
            .and_then(Json::as_array)
            && profiles.len() == 1
        {
            let id = profiles[0]
                .get("id")
                .and_then(Json::as_str)
                .ok_or_else(|| invalid("Missing profile ID"))?;
            self.delete(&resource_path("profiles", id)?).await?;
        }
        // Numeric serial comparison avoids Apple's inconsistent leading-zero filter.
        let certificates: Vec<_> = self
            .collection("v1/certificates", &[])
            .await?
            .into_iter()
            .filter(|c| {
                c.attribute("serialNumber")
                    .is_some_and(|s| signing_info.certificate.has_serial_number(s))
            })
            .map(|c| resource_id(&c.id, "certificates"))
            .collect();
        if certificates.is_empty() {
            return Err(invalid(
                "Signing certificate was not found in the developer account",
            ));
        }
        let devices: Vec<_> = self
            .collection("v1/devices", &[])
            .await?
            .into_iter()
            .filter(|d| matches!(d.attribute("deviceClass"), Some("IPAD" | "IPHONE" | "IPOD")))
            .map(|d| resource_id(&d.id, "devices"))
            .collect();
        let profile = self
            .create(
                "v1/profiles",
                json!({"type":"profiles", "attributes":{
            "name":format!("XTL profile {}",bundle.id), "profileType":"IOS_APP_DEVELOPMENT" },
            "relationships":{ "bundleId":{"data":resource_id(&bundle.id,"bundleIds")},
                "devices":{"data":devices}, "certificates":{"data":certificates} }}),
            )
            .await?;
        Mobileprovision::from_bytes(decode_content(&profile, "profileContent")?)
    }

    pub async fn provision_app(
        &self,
        app: &Path,
        signing_info: &SigningInfo,
        is_free: bool,
    ) -> Result<ProvisioningInfo> {
        let (bundle_id, executable) = crate::signer::bundle_metadata(app)?;
        // An unsigned executable legitimately has no entitlement blob. Other
        // malformed bundle metadata has already been checked above.
        let mut entitlements = Signer::analyze(&app.join(executable)).unwrap_or_default();
        if is_free {
            entitlements = entitlements.for_free_team()?;
        }
        let app_id = self.upsert_app(&bundle_id, &entitlements).await?;
        let new_bundle_id = app_id
            .attribute("identifier")
            .ok_or_else(|| invalid("Missing app identifier"))?
            .to_owned();
        entitlements.update(&signing_info.certificate.team_id()?, &new_bundle_id)?;
        entitlements.insert(Entitlement::GetTaskAllow(true))?;
        if let Some(groups) = entitlements.0.get("com.apple.security.application-groups") {
            let groups: Vec<String> = plist::from_value(groups)?;
            entitlements.insert(Entitlement::AppGroups(
                self.assign_app_groups(&groups, &app_id.id).await?,
            ))?;
        }
        let mobileprovision = self.fetch_profile(&new_bundle_id, signing_info).await?;
        Ok(ProvisioningInfo {
            new_bundle_id,
            entitlements,
            mobileprovision,
        })
    }
}

pub struct DeveloperServicesProvisioningOperation<'a> {
    pub context: &'a SigningContext,
}
impl DeveloperServicesProvisioningOperation<'_> {
    pub async fn perform(
        &self,
        app: &Path,
        confirm: &ConfirmRevocation,
        progress: &(dyn Fn(f64) + Send + Sync),
    ) -> Result<ProvisioningResponse> {
        progress(0.0);
        let client = &self.context.developer_api_client;
        client
            .register_device(self.context.target_device.as_ref())
            .await?;
        progress(1.0 / 3.0);
        let signing_info = client
            .fetch_certificate(self.context.signing_info_manager.as_ref(), confirm)
            .await?;
        progress(2.0 / 3.0);
        let is_free = client.team().await?.is_some_and(|t| t.is_free());
        let mut apps = vec![app.to_path_buf()];
        for directory in ["PlugIns", "Extensions"] {
            let directory = app.join(directory);
            if directory.is_dir() {
                for entry in std::fs::read_dir(directory)? {
                    let path = entry?.path();
                    if path.is_dir()
                        && path
                            .extension()
                            .is_some_and(|e| e.eq_ignore_ascii_case("appex"))
                    {
                        apps.push(path);
                    }
                }
            }
        }
        let mut provisioning = BTreeMap::new();
        // Sequential requests keep app group upserts deterministic across extensions.
        for app in apps {
            provisioning.insert(
                app.clone(),
                client.provision_app(&app, &signing_info, is_free).await?,
            );
        }
        progress(1.0);
        Ok(ProvisioningResponse {
            signing_info,
            provisioning,
        })
    }
}
