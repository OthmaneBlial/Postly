//! Secure storage for values that must not become Git-native project data.
//!
//! The persisted environment model stores only an opaque reference for a
//! keychain-backed variable. The value itself is kept in the operating system
//! credential store through `keyring`. A deterministic workspace namespace
//! lets a moved or copied workspace fail closed instead of accidentally
//! resolving a secret belonging to another project.

use std::{collections::BTreeMap, path::Path, sync::Arc};

#[cfg(test)]
use std::sync::Mutex;

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::model::{Environment, Variables};

const KEYRING_SERVICE: &str = "com.othmaneblial.postly";
const REFERENCE_PREFIX: &str = "keychain:v1";
// Keep the account identifier below common platform credential-attribute
// limits while retaining 128 bits per component.
const DIGEST_LENGTH: usize = 32;

#[derive(Debug, Error)]
pub enum SecretStoreError {
    #[error("secure storage backend failed: {0}")]
    Backend(String),
    #[error("secret is not present in secure storage: {reference}")]
    Missing { reference: String },
    #[error("invalid secure-storage reference: {0}")]
    InvalidReference(String),
    #[error("{field} cannot be empty when storing a secret")]
    EmptyField { field: &'static str },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretReference {
    value: String,
}

impl SecretReference {
    pub fn as_str(&self) -> &str {
        &self.value
    }

    pub fn into_string(self) -> String {
        self.value
    }
}

impl std::fmt::Display for SecretReference {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.value)
    }
}

#[derive(Clone)]
pub struct SecretStore {
    namespace: String,
    backend: Arc<dyn SecretBackend>,
}

impl SecretStore {
    /// Create a store scoped to one workspace using the platform credential
    /// store. The workspace path is hashed and never used as a credential
    /// value or account name.
    pub fn for_workspace(root: impl AsRef<Path>) -> Self {
        Self {
            namespace: workspace_namespace(root.as_ref()),
            backend: Arc::new(KeyringBackend),
        }
    }

    /// Return the opaque reference that will identify one environment value.
    /// This does not access the keychain.
    pub fn reference_for_environment(
        &self,
        environment_name: &str,
        key: &str,
    ) -> Result<SecretReference, SecretStoreError> {
        if environment_name.trim().is_empty() {
            return Err(SecretStoreError::EmptyField {
                field: "environment name",
            });
        }
        if key.trim().is_empty() {
            return Err(SecretStoreError::EmptyField {
                field: "variable key",
            });
        }
        Ok(SecretReference {
            value: format!(
                "{REFERENCE_PREFIX}:{}:{}:{}",
                self.namespace,
                digest_hex(environment_name.trim()),
                digest_hex(key.trim())
            ),
        })
    }

    /// Store an environment value in the platform credential store and return
    /// the reference safe to persist in a Git-native environment file.
    pub fn set_environment_secret(
        &self,
        environment_name: &str,
        key: &str,
        value: &str,
    ) -> Result<SecretReference, SecretStoreError> {
        let reference = self.reference_for_environment(environment_name, key)?;
        self.backend
            .set(reference.as_str(), value)
            .map_err(SecretStoreError::Backend)?;
        Ok(reference)
    }

    /// Resolve a previously stored environment value. References from another
    /// workspace namespace are rejected before the backend is consulted.
    pub fn get(&self, reference: &str) -> Result<String, SecretStoreError> {
        self.validate_reference(reference)?;
        match self.backend.get(reference) {
            Ok(value) => Ok(value),
            Err(BackendError::Missing) => Err(SecretStoreError::Missing {
                reference: reference.to_owned(),
            }),
            Err(BackendError::Failure(message)) => Err(SecretStoreError::Backend(message)),
        }
    }

    /// Delete a keychain entry. This is intentionally explicit so replacing a
    /// secret with a plain value does not silently destroy a credential.
    pub fn delete(&self, reference: &str) -> Result<(), SecretStoreError> {
        self.validate_reference(reference)?;
        match self.backend.delete(reference) {
            Ok(()) | Err(BackendError::Missing) => Ok(()),
            Err(BackendError::Failure(message)) => Err(SecretStoreError::Backend(message)),
        }
    }

    /// Resolve enabled environment values, loading keychain-backed entries and
    /// preserving legacy plaintext variables for backwards compatibility.
    pub fn resolve_environment(
        &self,
        environment: &Environment,
    ) -> Result<Variables, SecretStoreError> {
        let mut values = BTreeMap::new();
        for (key, variable) in &environment.variables {
            if !variable.enabled {
                continue;
            }
            let value = match variable.secret_ref.as_deref() {
                Some(reference) => self.get(reference).map_err(|error| match error {
                    SecretStoreError::Missing { .. } => SecretStoreError::Missing {
                        reference: format!("{reference} (environment variable {key})"),
                    },
                    other => other,
                })?,
                None => variable.value.clone(),
            };
            values.insert(key.clone(), value);
        }
        Ok(values)
    }

    fn validate_reference(&self, reference: &str) -> Result<(), SecretStoreError> {
        let parts = reference.split(':').collect::<Vec<_>>();
        let valid = parts.len() == 5
            && parts[0] == "keychain"
            && parts[1] == "v1"
            && parts[2] == self.namespace
            && parts[3].len() == DIGEST_LENGTH
            && parts[4].len() == DIGEST_LENGTH
            && parts[3].bytes().all(|byte| byte.is_ascii_hexdigit())
            && parts[4].bytes().all(|byte| byte.is_ascii_hexdigit());
        if valid {
            Ok(())
        } else {
            Err(SecretStoreError::InvalidReference(reference.to_owned()))
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(root: impl AsRef<Path>) -> Self {
        Self {
            namespace: workspace_namespace(root.as_ref()),
            backend: Arc::new(MemoryBackend::default()),
        }
    }
}

trait SecretBackend: Send + Sync {
    fn set(&self, reference: &str, value: &str) -> Result<(), String>;
    fn get(&self, reference: &str) -> Result<String, BackendError>;
    fn delete(&self, reference: &str) -> Result<(), BackendError>;
}

struct KeyringBackend;

impl SecretBackend for KeyringBackend {
    fn set(&self, reference: &str, value: &str) -> Result<(), String> {
        let entry =
            keyring::Entry::new(KEYRING_SERVICE, reference).map_err(|error| error.to_string())?;
        entry.set_password(value).map_err(|error| error.to_string())
    }

    fn get(&self, reference: &str) -> Result<String, BackendError> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, reference)
            .map_err(|error| BackendError::Failure(error.to_string()))?;
        entry.get_password().map_err(keyring_error)
    }

    fn delete(&self, reference: &str) -> Result<(), BackendError> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, reference)
            .map_err(|error| BackendError::Failure(error.to_string()))?;
        entry.delete_credential().map_err(keyring_error)
    }
}

fn keyring_error(error: keyring::Error) -> BackendError {
    match error {
        keyring::Error::NoEntry => BackendError::Missing,
        other => BackendError::Failure(other.to_string()),
    }
}

#[derive(Debug)]
enum BackendError {
    Missing,
    Failure(String),
}

fn workspace_namespace(root: &Path) -> String {
    let path = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    digest_hex(&path.to_string_lossy())
}

fn digest_hex(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    digest
        .iter()
        .take(DIGEST_LENGTH / 2)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
#[derive(Default)]
struct MemoryBackend {
    values: Mutex<BTreeMap<String, String>>,
}

#[cfg(test)]
impl SecretBackend for MemoryBackend {
    fn set(&self, reference: &str, value: &str) -> Result<(), String> {
        self.values
            .lock()
            .map_err(|_| "memory backend lock poisoned".to_owned())?
            .insert(reference.to_owned(), value.to_owned());
        Ok(())
    }

    fn get(&self, reference: &str) -> Result<String, BackendError> {
        self.values
            .lock()
            .map_err(|_| BackendError::Failure("memory backend lock poisoned".to_owned()))?
            .get(reference)
            .cloned()
            .ok_or(BackendError::Missing)
    }

    fn delete(&self, reference: &str) -> Result<(), BackendError> {
        let removed = self
            .values
            .lock()
            .map_err(|_| BackendError::Failure("memory backend lock poisoned".to_owned()))?
            .remove(reference);
        if removed.is_some() {
            Ok(())
        } else {
            Err(BackendError::Missing)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Environment, EnvironmentVariable};

    #[test]
    fn references_are_opaque_and_workspace_scoped() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = SecretStore::for_test(directory.path());
        let reference = store
            .reference_for_environment("Local", "API_TOKEN")
            .expect("reference");

        assert!(reference.as_str().starts_with("keychain:v1:"));
        assert!(!reference.as_str().contains("Local"));
        assert!(!reference.as_str().contains("API_TOKEN"));

        let other_directory = tempfile::tempdir().expect("other");
        let other = SecretStore::for_test(other_directory.path());
        assert!(matches!(
            other.get(reference.as_str()),
            Err(SecretStoreError::InvalidReference(_))
        ));
    }

    #[test]
    fn stores_and_resolves_environment_secrets_without_persisting_the_value() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = SecretStore::for_test(directory.path());
        let reference = store
            .set_environment_secret("Local", "API_TOKEN", "do-not-persist")
            .expect("set secret");
        let mut environment = Environment::new("Local");
        environment.variables.insert(
            "API_TOKEN".to_owned(),
            EnvironmentVariable::keychain(reference.into_string()),
        );

        let resolved = store.resolve_environment(&environment).expect("resolve");
        assert_eq!(resolved["API_TOKEN"], "do-not-persist");
        let serialized = toml::to_string(&environment).expect("serialize");
        assert!(!serialized.contains("do-not-persist"));
        assert!(serialized.contains("secret_ref"));
    }

    #[test]
    fn deleting_a_missing_secret_is_idempotent() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = SecretStore::for_test(directory.path());
        let reference = store
            .reference_for_environment("Local", "API_TOKEN")
            .expect("reference");

        store.delete(reference.as_str()).expect("delete missing");
        assert!(matches!(
            store.get(reference.as_str()),
            Err(SecretStoreError::Missing { .. })
        ));
    }
}
