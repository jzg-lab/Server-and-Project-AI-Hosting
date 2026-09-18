use std::path::{Path, PathBuf};

use chrono::{SecondsFormat, Utc};
use thiserror::Error;
use tokio::{fs, io::AsyncWriteExt};
use uuid::Uuid;

const SSH_KEY_PREFIX: &str = "secret://ssh/";
const SSH_PASSWORD_PREFIX: &str = "secret://ssh-password/";
const MODEL_KEY_PREFIX: &str = "secret://model/";
const MAX_SSH_KEY_BYTES: usize = 128 * 1024;
const MAX_SSH_PASSWORD_BYTES: usize = 4 * 1024;
const MAX_MODEL_KEY_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone)]
pub struct FileSecretStore {
    root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSecretRef {
    pub credential_ref: String,
    pub created_at: String,
}

#[derive(Debug, Error)]
pub enum SecretStoreError {
    #[error("invalid SSH private key")]
    InvalidKey,
    #[error("invalid model API key")]
    InvalidModelKey,
    #[error("invalid SSH password")]
    InvalidPassword,
    #[error("invalid credential reference")]
    InvalidReference,
    #[error("credential reference not found")]
    NotFound,
    #[error("secret storage unavailable")]
    Io(#[from] std::io::Error),
}

impl FileSecretStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub async fn store_ssh_key(
        &self,
        private_key: &str,
    ) -> Result<StoredSecretRef, SecretStoreError> {
        validate_private_key(private_key)?;
        self.ensure_root().await?;

        let id = Uuid::new_v4();
        let path = self.root.join(format!("{id}.key"));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = options.open(&path).await?;
        if let Err(error) = file.write_all(private_key.as_bytes()).await {
            let _ = fs::remove_file(&path).await;
            return Err(error.into());
        }
        file.flush().await?;
        drop(file);
        set_private_file_permissions(&path).await?;

        Ok(StoredSecretRef {
            credential_ref: format!("{SSH_KEY_PREFIX}{id}"),
            created_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        })
    }

    pub async fn store_model_key(
        &self,
        api_key: &str,
    ) -> Result<StoredSecretRef, SecretStoreError> {
        let api_key = api_key.trim();
        if api_key.is_empty()
            || api_key.len() > MAX_MODEL_KEY_BYTES
            || api_key.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(SecretStoreError::InvalidModelKey);
        }
        self.ensure_root().await?;
        let id = Uuid::new_v4();
        let path = self.root.join(format!("{id}.secret"));
        self.write_secret(&path, api_key.as_bytes()).await?;
        Ok(StoredSecretRef {
            credential_ref: format!("{MODEL_KEY_PREFIX}{id}"),
            created_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        })
    }

    pub async fn store_ssh_password(
        &self,
        password: &str,
    ) -> Result<StoredSecretRef, SecretStoreError> {
        if password.is_empty()
            || password.len() > MAX_SSH_PASSWORD_BYTES
            || password.contains(['\0', '\r', '\n'])
        {
            return Err(SecretStoreError::InvalidPassword);
        }
        self.ensure_root().await?;
        let id = Uuid::new_v4();
        let path = self.root.join(format!("{id}.password"));
        self.write_secret(&path, password.as_bytes()).await?;
        Ok(StoredSecretRef {
            credential_ref: format!("{SSH_PASSWORD_PREFIX}{id}"),
            created_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        })
    }

    pub async fn resolve_ssh_key(&self, credential_ref: &str) -> Result<PathBuf, SecretStoreError> {
        let id = parse_reference(credential_ref, SSH_KEY_PREFIX)?;
        let path = self.root.join(format!("{id}.key"));
        match fs::metadata(&path).await {
            Ok(metadata) if metadata.is_file() => Ok(path),
            Ok(_) => Err(SecretStoreError::NotFound),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(SecretStoreError::NotFound)
            }
            Err(error) => Err(error.into()),
        }
    }

    pub async fn resolve_model_key(
        &self,
        credential_ref: &str,
    ) -> Result<String, SecretStoreError> {
        let id = parse_reference(credential_ref, MODEL_KEY_PREFIX)?;
        let path = self.root.join(format!("{id}.secret"));
        match fs::read_to_string(path).await {
            Ok(value) if !value.is_empty() => Ok(value),
            Ok(_) => Err(SecretStoreError::NotFound),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(SecretStoreError::NotFound)
            }
            Err(error) => Err(error.into()),
        }
    }

    pub async fn resolve_ssh_password(
        &self,
        credential_ref: &str,
    ) -> Result<String, SecretStoreError> {
        let id = parse_reference(credential_ref, SSH_PASSWORD_PREFIX)?;
        let path = self.root.join(format!("{id}.password"));
        match fs::read_to_string(path).await {
            Ok(value) if !value.is_empty() => Ok(value),
            Ok(_) => Err(SecretStoreError::NotFound),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(SecretStoreError::NotFound)
            }
            Err(error) => Err(error.into()),
        }
    }

    pub async fn contains(&self, credential_ref: &str) -> bool {
        if credential_ref.starts_with(SSH_KEY_PREFIX) {
            self.resolve_ssh_key(credential_ref).await.is_ok()
        } else if credential_ref.starts_with(SSH_PASSWORD_PREFIX) {
            self.resolve_ssh_password(credential_ref).await.is_ok()
        } else {
            false
        }
    }

    pub async fn delete(&self, credential_ref: &str) -> Result<(), SecretStoreError> {
        let (id, extension) = if credential_ref.starts_with(SSH_KEY_PREFIX) {
            (parse_reference(credential_ref, SSH_KEY_PREFIX)?, "key")
        } else if credential_ref.starts_with(SSH_PASSWORD_PREFIX) {
            (
                parse_reference(credential_ref, SSH_PASSWORD_PREFIX)?,
                "password",
            )
        } else {
            (parse_reference(credential_ref, MODEL_KEY_PREFIX)?, "secret")
        };
        let path = self.root.join(format!("{id}.{extension}"));
        match fs::remove_file(path).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    async fn ensure_root(&self) -> Result<(), SecretStoreError> {
        fs::create_dir_all(&self.root).await?;
        set_private_directory_permissions(&self.root).await?;
        Ok(())
    }

    async fn write_secret(&self, path: &Path, value: &[u8]) -> Result<(), SecretStoreError> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = options.open(path).await?;
        if let Err(error) = file.write_all(value).await {
            let _ = fs::remove_file(path).await;
            return Err(error.into());
        }
        file.flush().await?;
        drop(file);
        set_private_file_permissions(path).await?;
        Ok(())
    }
}

fn validate_private_key(private_key: &str) -> Result<(), SecretStoreError> {
    let bytes = private_key.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_SSH_KEY_BYTES || private_key.contains('\0') {
        return Err(SecretStoreError::InvalidKey);
    }
    let formats = [
        (
            "-----BEGIN OPENSSH PRIVATE KEY-----",
            "-----END OPENSSH PRIVATE KEY-----",
        ),
        ("-----BEGIN PRIVATE KEY-----", "-----END PRIVATE KEY-----"),
        (
            "-----BEGIN RSA PRIVATE KEY-----",
            "-----END RSA PRIVATE KEY-----",
        ),
        (
            "-----BEGIN EC PRIVATE KEY-----",
            "-----END EC PRIVATE KEY-----",
        ),
    ];
    if !formats
        .iter()
        .any(|(begin, end)| private_key.starts_with(begin) && private_key.trim_end().ends_with(end))
    {
        return Err(SecretStoreError::InvalidKey);
    }
    Ok(())
}

fn parse_reference(credential_ref: &str, prefix: &str) -> Result<Uuid, SecretStoreError> {
    let value = credential_ref
        .strip_prefix(prefix)
        .ok_or(SecretStoreError::InvalidReference)?;
    Uuid::parse_str(value).map_err(|_| SecretStoreError::InvalidReference)
}

#[cfg(unix)]
async fn set_private_file_permissions(path: &Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await
}

#[cfg(windows)]
async fn set_private_file_permissions(path: &Path) -> Result<(), std::io::Error> {
    let user =
        std::env::var("USERNAME").map_err(|_| std::io::Error::other("USERNAME is unavailable"))?;
    let grant = format!("{user}:(F)");
    let status = tokio::process::Command::new("icacls")
        .arg(path)
        .arg("/inheritance:r")
        .arg("/grant:r")
        .arg(grant)
        .arg("/Q")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other("could not restrict SSH key ACL"))
    }
}

#[cfg(not(any(unix, windows)))]
async fn set_private_file_permissions(_path: &Path) -> Result<(), std::io::Error> {
    Ok(())
}

#[cfg(unix)]
async fn set_private_directory_permissions(path: &Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).await
}

#[cfg(not(unix))]
async fn set_private_directory_permissions(_path: &Path) -> Result<(), std::io::Error> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[tokio::test]
    async fn rejects_non_key_material_without_writing_it() {
        let directory = TempDir::new().unwrap();
        let store = FileSecretStore::new(directory.path());
        let error = store.store_ssh_key("TOKEN=value").await.unwrap_err();
        assert!(matches!(error, SecretStoreError::InvalidKey));
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn rejects_truncated_private_key_envelopes() {
        let directory = TempDir::new().unwrap();
        let store = FileSecretStore::new(directory.path());
        let error = store
            .store_ssh_key("-----BEGIN OPENSSH PRIVATE KEY-----\ntruncated")
            .await
            .unwrap_err();
        assert!(matches!(error, SecretStoreError::InvalidKey));
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn model_keys_use_a_distinct_reference_and_are_read_server_side() {
        let directory = TempDir::new().unwrap();
        let store = FileSecretStore::new(directory.path());
        let stored = store.store_model_key("fixture-model-key").await.unwrap();
        assert!(stored.credential_ref.starts_with(MODEL_KEY_PREFIX));
        assert_eq!(
            store
                .resolve_model_key(&stored.credential_ref)
                .await
                .unwrap(),
            "fixture-model-key"
        );
        assert!(store.resolve_ssh_key(&stored.credential_ref).await.is_err());
    }

    #[tokio::test]
    async fn ssh_passwords_use_a_distinct_reference_and_are_read_server_side() {
        let directory = TempDir::new().unwrap();
        let store = FileSecretStore::new(directory.path());
        let stored = store.store_ssh_password("fixture-password").await.unwrap();
        assert!(stored.credential_ref.starts_with(SSH_PASSWORD_PREFIX));
        assert_eq!(
            store
                .resolve_ssh_password(&stored.credential_ref)
                .await
                .unwrap(),
            "fixture-password"
        );
        assert!(store.resolve_ssh_key(&stored.credential_ref).await.is_err());
    }
}
