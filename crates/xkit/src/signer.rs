use crate::{
    Result,
    developer_services::{
        ConfirmRevocation, DeveloperAPIClient, DeveloperServicesProvisioningOperation,
    },
    entitlements::Entitlements,
    invalid,
    model::SigningInfo,
    utilities::SigningInfoManager,
};
use apple_codesign::{
    SettingsScope, SigningSettings, UnifiedSigner, cryptography::InMemoryPrivateKey,
};
use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TargetDevice {
    pub udid: String,
    pub name: String,
}

#[derive(Clone)]
pub struct SigningContext {
    pub developer_api_client: DeveloperAPIClient,
    pub target_device: Option<TargetDevice>,
    pub signing_info_manager: Arc<dyn SigningInfoManager>,
}
impl SigningContext {
    pub fn new(
        developer_api_client: DeveloperAPIClient,
        signing_info_manager: Arc<dyn SigningInfoManager>,
    ) -> Self {
        Self {
            developer_api_client,
            signing_info_manager,
            target_device: None,
        }
    }
    pub fn host_name() -> String {
        std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .unwrap_or_else(|_| "XTool Client".into())
    }
}

pub(crate) fn bundle_metadata(app: &Path) -> Result<(String, String)> {
    let info: plist::Dictionary = plist::from_file(app.join("Info.plist"))?;
    let string = |key| {
        info.get(key)
            .and_then(plist::Value::as_string)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| invalid(format!("Missing {key} in {}", app.display())))
    };
    let executable = string("CFBundleExecutable")?;
    if Path::new(&executable).components().count() != 1
        || !matches!(
            Path::new(&executable).components().next(),
            Some(Component::Normal(_))
        )
    {
        return Err(invalid("Bundle executable must be a file name"));
    }
    if !app.join(&executable).is_file() {
        return Err(invalid(format!("Missing executable in {}", app.display())));
    }
    Ok((string("CFBundleIdentifier")?, executable))
}

pub enum SigningIdentity<'a> {
    Real(&'a SigningInfo),
    Adhoc,
}
pub struct Signer;
impl Signer {
    pub fn name() -> &'static str {
        "apple-codesign"
    }
    pub fn analyze(executable: &Path) -> Result<Entitlements> {
        Ok(Entitlements(
            plume_core::MachO::new(executable)?
                .entitlements()
                .clone()
                .unwrap_or_default(),
        ))
    }
    pub fn sign(
        app: &Path,
        identity: SigningIdentity<'_>,
        entitlement_mapping: &BTreeMap<PathBuf, Entitlements>,
        progress: &(dyn Fn(Option<f64>) + Send + Sync),
    ) -> Result<()> {
        progress(None);
        let key;
        let mut settings = SigningSettings::default();
        if let SigningIdentity::Real(info) = identity {
            let pem =
                pem::parse(info.private_key.data.as_ref()).map_err(|e| invalid(e.to_string()))?;
            key = match pem.tag() {
                "PRIVATE KEY" => InMemoryPrivateKey::from_pkcs8_der(pem.contents())?,
                "RSA PRIVATE KEY" => InMemoryPrivateKey::from_pkcs1_der(pem.contents())?,
                _ => return Err(invalid("Expected a PEM private key")),
            };
            settings.set_signing_key(&key, info.certificate.raw().clone());
            settings.chain_apple_certificates();
            settings.set_team_id_from_signing_certificate();
        }
        // Use the bundle library's exact scope strings. On Windows, nested
        // bundle scopes can contain both native separators and `/` delimiters.
        let nested = apple_bundles::DirectoryBundle::new_from_path(app)
            .and_then(|bundle| bundle.nested_bundles(true))
            .map_err(|error| invalid(error.to_string()))?;
        for (path, entitlements) in entitlement_mapping {
            let relative = path
                .strip_prefix(app)
                .map_err(|_| invalid("Entitlement mapping is outside the app"))?;
            let scope = if relative.as_os_str().is_empty() {
                SettingsScope::Main
            } else {
                let (scope, _) = nested
                    .iter()
                    .find(|(scope, _)| Path::new(scope) == relative)
                    .ok_or_else(|| {
                        invalid(format!("No nested signing bundle at {}", path.display()))
                    })?;
                SettingsScope::Path(scope.clone())
            };
            let xml =
                String::from_utf8(entitlements.to_xml()?).map_err(|e| invalid(e.to_string()))?;
            settings.set_entitlements_xml(scope, xml)?;
        }
        UnifiedSigner::new(settings).sign_path_in_place(app)?;
        progress(Some(1.0));
        Ok(())
    }
}

pub struct AutoSigner {
    pub context: SigningContext,
}
impl AutoSigner {
    pub async fn sign(
        &self,
        app: &Path,
        confirm: &ConfirmRevocation,
        status: &(dyn Fn(&str) + Send + Sync),
        progress: &(dyn Fn(Option<f64>) + Send + Sync),
        did_provision: &(dyn Fn() -> Result<()> + Send + Sync),
    ) -> Result<String> {
        status("Provisioning");
        let response = DeveloperServicesProvisioningOperation {
            context: &self.context,
        }
        .perform(app, confirm, &|value| progress(Some(value)))
        .await?;
        let main_id = response
            .provisioning
            .get(app)
            .ok_or_else(|| invalid("Missing main app provisioning"))?
            .new_bundle_id
            .clone();
        did_provision()?;
        let mut entitlements = BTreeMap::new();
        for (path, info) in response.provisioning {
            let info_path = path.join("Info.plist");
            let mut plist: plist::Dictionary = plist::from_file(&info_path)?;
            plist.insert("CFBundleIdentifier".into(), info.new_bundle_id.into());
            plist::to_file_xml(info_path, &plist)?;
            fs::write(
                path.join("embedded.mobileprovision"),
                info.mobileprovision.data(),
            )?;
            entitlements.insert(path, info.entitlements);
        }
        status("Signing");
        Signer::sign(
            app,
            SigningIdentity::Real(&response.signing_info),
            &entitlements,
            progress,
        )?;
        Ok(main_id)
    }
}
