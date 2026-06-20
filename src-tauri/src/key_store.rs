use crate::{
    claude_auth, official_auth,
    types::{KeyStatus, Provider, ProviderKind},
};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

const STORE_FILE_NAME: &str = "secrets.json";

type SecretMap = BTreeMap<String, String>;

#[derive(Debug, Clone)]
pub struct KeyStore {
    path: PathBuf,
    lock: Arc<Mutex<()>>,
}

impl KeyStore {
    pub fn new(app_data_dir: &Path) -> Self {
        Self {
            path: app_data_dir.join(STORE_FILE_NAME),
            lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn set_secret(&self, key_ref: &str, value: &str) -> Result<(), String> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| "Local key store lock was poisoned".to_string())?;
        let mut secrets = self.read_secrets_locked()?;
        secrets.insert(key_ref.to_string(), value.to_string());
        self.write_secrets_locked(&secrets)
    }

    pub fn get_secret(&self, key_ref: &str) -> Result<Option<String>, String> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| "Local key store lock was poisoned".to_string())?;
        Ok(self.read_secrets_locked()?.get(key_ref).cloned())
    }

    pub fn delete_secret(&self, key_ref: &str) -> Result<(), String> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| "Local key store lock was poisoned".to_string())?;
        let mut secrets = self.read_secrets_locked()?;
        secrets.remove(key_ref);
        self.write_secrets_locked(&secrets)
    }

    pub fn status_for_provider(&self, provider: &Provider) -> KeyStatus {
        if provider.kind == ProviderKind::OfficialAnthropicCli {
            let (present, available, message) = claude_auth::cli_status();
            return KeyStatus {
                provider_id: provider.id.clone(),
                present,
                available,
                message,
            };
        }

        if provider.kind == ProviderKind::OfficialAnthropicDesktop {
            let (present, available, message) = claude_auth::desktop_status();
            return KeyStatus {
                provider_id: provider.id.clone(),
                present,
                available,
                message,
            };
        }

        if matches!(
            provider.kind,
            ProviderKind::OfficialOpenAiAccount | ProviderKind::OfficialAnthropicAccount
        ) {
            return official_auth::status_for_provider(provider);
        }

        let Some(key_ref) = provider.key_ref.as_deref() else {
            return KeyStatus {
                provider_id: provider.id.clone(),
                present: false,
                available: true,
                message: None,
            };
        };

        match self.get_secret(key_ref) {
            Ok(Some(_)) => KeyStatus {
                provider_id: provider.id.clone(),
                present: true,
                available: true,
                message: None,
            },
            Ok(None) => KeyStatus {
                provider_id: provider.id.clone(),
                present: false,
                available: true,
                message: None,
            },
            Err(message) => KeyStatus {
                provider_id: provider.id.clone(),
                present: false,
                available: false,
                message: Some(message),
            },
        }
    }

    fn read_secrets_locked(&self) -> Result<SecretMap, String> {
        if !self.path.exists() {
            return Ok(SecretMap::new());
        }
        let raw = fs::read_to_string(&self.path)
            .map_err(|error| format!("Could not read local key store: {error}"))?;
        if raw.trim().is_empty() {
            return Ok(SecretMap::new());
        }
        serde_json::from_str::<SecretMap>(&raw)
            .map_err(|error| format!("Could not parse local key store: {error}"))
    }

    fn write_secrets_locked(&self, secrets: &SecretMap) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("Could not create local key store directory: {error}"))?;
        }
        let mut file = open_secret_file(&self.path)?;
        serde_json::to_writer_pretty(&mut file, secrets)
            .map_err(|error| format!("Could not write local key store: {error}"))?;
        file.write_all(b"\n")
            .map_err(|error| format!("Could not finish local key store write: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("Could not sync local key store: {error}"))
    }
}

#[cfg(unix)]
fn open_secret_file(path: &Path) -> Result<fs::File, String> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| format!("Could not open local key store: {error}"))?;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("Could not protect local key store permissions: {error}"))?;
    Ok(file)
}

#[cfg(not(unix))]
fn open_secret_file(path: &Path) -> Result<fs::File, String> {
    fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
        .map_err(|error| format!("Could not open local key store: {error}"))
}

#[cfg(test)]
mod tests {
    use super::KeyStore;

    #[test]
    fn stores_reads_and_deletes_secrets_without_system_keychain() {
        let temp = tempfile::tempdir().unwrap();
        let store = KeyStore::new(temp.path());

        store.set_secret("provider:test", "sk-test").unwrap();
        assert_eq!(
            store.get_secret("provider:test").unwrap().as_deref(),
            Some("sk-test")
        );

        let raw = std::fs::read_to_string(temp.path().join("secrets.json")).unwrap();
        assert!(raw.contains("provider:test"));
        assert!(raw.contains("sk-test"));

        store.delete_secret("provider:test").unwrap();
        assert!(store.get_secret("provider:test").unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn secret_file_is_owner_only_on_unix() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let store = KeyStore::new(temp.path());
        store.set_secret("provider:test", "sk-test").unwrap();

        let mode = std::fs::metadata(temp.path().join("secrets.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}
