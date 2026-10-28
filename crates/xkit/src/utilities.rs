use crate::{Result, invalid, model::SigningInfo};
use std::{
    collections::HashMap,
    fs,
    path::{Component, Path, PathBuf},
    sync::Mutex,
};
mod zip;
pub use zip::{ZIPCompressor, copy_directory};

#[derive(Debug)]
pub struct ErrorList<E> {
    pub errors: Vec<E>,
}
impl<E: std::fmt::Display> std::fmt::Display for ErrorList<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (index, error) in self.errors.iter().enumerate() {
            if index != 0 {
                f.write_str("; ")?;
            }
            std::fmt::Display::fmt(error, f)?;
        }
        Ok(())
    }
}
impl<E: std::error::Error> std::error::Error for ErrorList<E> {}

pub struct ProvisioningIdentifiers;
impl ProvisioningIdentifiers {
    pub const ID_PREFIX: &str = "XTL-";
    pub const GROUP_PREFIX: &str = "group.";
    pub fn sanitize(identifier: &str) -> String {
        if identifier.starts_with(Self::ID_PREFIX) {
            identifier.split('.').skip(1).collect::<Vec<_>>().join(".")
        } else {
            identifier.into()
        }
    }
    pub fn identifier(sanitized: &str, identity_id: &str) -> String {
        format!(
            "XTL-{}.{}",
            identity_id
                .split('-')
                .next()
                .unwrap_or(identity_id)
                .to_uppercase(),
            sanitized
        )
    }
    pub fn sanitize_group(group: &str) -> String {
        Self::sanitize(group.strip_prefix("group.").unwrap_or(group))
    }
    pub fn group_id(sanitized: &str, identity_id: &str) -> String {
        format!("group.{}", Self::identifier(sanitized, identity_id))
    }
    pub fn app_name(sanitized: &str) -> String {
        format!("XTool {}", sanitized.replace('.', " "))
    }
    pub fn group_name(sanitized: &str) -> String {
        format!("XTool group {}", sanitized.replace('.', " "))
    }
}

pub fn persistent_directory() -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .ok_or_else(|| invalid("Home directory is unavailable"))?;
    Ok(PathBuf::from(home).join(".config").join("xtool"))
}

pub trait KeyValueStorage: Send + Sync {
    fn data(&self, key: &str) -> Result<Option<Vec<u8>>>;
    fn set_data(&self, key: &str, data: Option<&[u8]>) -> Result<()>;
    fn string(&self, key: &str) -> Result<Option<String>> {
        self.data(key)?
            .map(|bytes| String::from_utf8(bytes).map_err(|e| invalid(e.to_string())))
            .transpose()
    }
    fn set_string(&self, key: &str, value: Option<&str>) -> Result<()> {
        self.set_data(key, value.map(str::as_bytes))
    }
}

#[derive(Default)]
pub struct MemoryKeyValueStorage(Mutex<HashMap<String, Vec<u8>>>);
impl KeyValueStorage for MemoryKeyValueStorage {
    fn data(&self, key: &str) -> Result<Option<Vec<u8>>> {
        Ok(self
            .0
            .lock()
            .map_err(|_| invalid("Storage lock poisoned"))?
            .get(key)
            .cloned())
    }
    fn set_data(&self, key: &str, data: Option<&[u8]>) -> Result<()> {
        let mut values = self
            .0
            .lock()
            .map_err(|_| invalid("Storage lock poisoned"))?;
        if let Some(data) = data {
            values.insert(key.into(), data.into());
        } else {
            values.remove(key);
        }
        Ok(())
    }
}

pub struct DirectoryStorage {
    base: PathBuf,
    lock: Mutex<()>,
}
impl DirectoryStorage {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            base: directory.into().join("data"),
            lock: Mutex::new(()),
        }
    }
    fn path(&self, key: &str) -> Result<PathBuf> {
        if key.is_empty()
            || key.contains(['\\', ':'])
            || Path::new(key)
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(invalid(
                "Storage keys must be relative paths without parent components",
            ));
        }
        Ok(self.base.join(key))
    }
}
impl KeyValueStorage for DirectoryStorage {
    fn data(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| invalid("Storage lock poisoned"))?;
        match fs::read(self.path(key)?) {
            Ok(data) => Ok(Some(data)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    fn set_data(&self, key: &str, data: Option<&[u8]>) -> Result<()> {
        use std::io::Write;
        let _guard = self
            .lock
            .lock()
            .map_err(|_| invalid("Storage lock poisoned"))?;
        let path = self.path(key)?;
        if let Some(data) = data {
            let parent = path
                .parent()
                .ok_or_else(|| invalid("Invalid storage path"))?;
            fs::create_dir_all(parent)?;
            let mut file = tempfile::NamedTempFile::new_in(parent)?;
            file.write_all(data)?;
            file.persist(&path).map_err(|e| e.error)?;
        } else {
            match fs::remove_file(path) {
                Ok(()) => (),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
}

pub trait SigningInfoManager: Send + Sync {
    fn info(&self, identity_id: &str) -> Result<Option<SigningInfo>>;
    fn set_info(&self, identity_id: &str, info: Option<&SigningInfo>) -> Result<()>;
}
pub struct KeyValueSigningInfoManager {
    pub storage: std::sync::Arc<dyn KeyValueStorage>,
}
impl SigningInfoManager for KeyValueSigningInfoManager {
    fn info(&self, identity_id: &str) -> Result<Option<SigningInfo>> {
        self.storage
            .data(identity_id)?
            .map(|data| Ok(plist::from_bytes(&data)?))
            .transpose()
    }
    fn set_info(&self, identity_id: &str, info: Option<&SigningInfo>) -> Result<()> {
        let data = info
            .map(|info| {
                let mut bytes = Vec::new();
                plist::to_writer_binary(&mut bytes, info).map(|_| bytes)
            })
            .transpose()?;
        self.storage.set_data(identity_id, data.as_deref())
    }
}
impl Default for KeyValueSigningInfoManager {
    fn default() -> Self {
        Self {
            storage: std::sync::Arc::new(MemoryKeyValueStorage::default()),
        }
    }
}

/// Native Apple data-protection keychain storage. Other platforms can use
/// DirectoryStorage or supply their own KeyValueStorage implementation.
#[cfg(target_vendor = "apple")]
pub struct KeychainStorage {
    pub service: String,
    lock: Mutex<()>,
}

#[cfg(target_vendor = "apple")]
impl KeychainStorage {
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
            lock: Mutex::new(()),
        }
    }
    fn options(&self, key: &str) -> security_framework::passwords::PasswordOptions {
        let mut options = security_framework::passwords::PasswordOptions::new_generic_password(
            &self.service,
            key,
        );
        options.use_protected_keychain();
        options
    }
}

#[cfg(target_vendor = "apple")]
impl KeyValueStorage for KeychainStorage {
    fn data(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| invalid("Keychain lock poisoned"))?;
        match security_framework::passwords::generic_password(self.options(key)) {
            Ok(data) => Ok(Some(data)),
            Err(error) if error.code() == -25300 /* errSecItemNotFound */ => Ok(None),
            Err(error) => Err(invalid(format!("Keychain error: {error}"))),
        }
    }
    fn set_data(&self, key: &str, data: Option<&[u8]>) -> Result<()> {
        use security_framework::{
            access_control::{ProtectionMode, SecAccessControl},
            passwords,
        };
        let _guard = self
            .lock
            .lock()
            .map_err(|_| invalid("Keychain lock poisoned"))?;
        // Remove before replacing to ensure accessibility attributes are updated.
        match passwords::delete_generic_password_options(self.options(key)) {
            Ok(()) => (),
            Err(error) if error.code() == -25300 => (),
            Err(error) => return Err(invalid(format!("Keychain error: {error}"))),
        }
        if let Some(data) = data {
            let mut options = self.options(key);
            options.set_access_control(
                SecAccessControl::create_with_protection(
                    Some(ProtectionMode::AccessibleAfterFirstUnlock),
                    0,
                )
                .map_err(|error| invalid(format!("Keychain error: {error}")))?,
            );
            passwords::set_generic_password_options(data, options)
                .map_err(|error| invalid(format!("Keychain error: {error}")))?;
        }
        Ok(())
    }
}
