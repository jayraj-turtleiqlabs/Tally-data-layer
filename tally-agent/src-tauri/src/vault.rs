use std::fs;
use std::path::PathBuf;
use keyring::Entry;

use crate::errors::VaultError;

const SERVICE_NAME: &str = "com.fininsight.tally-agent";
const TOKEN_ACCOUNT: &str = "agent-token";
const CONNECTION_ID_ACCOUNT: &str = "connection-id";

fn fallback_token_path() -> Option<PathBuf> {
    dirs::data_local_dir().map(|d| d.join("fininsight-tally-agent").join(".token"))
}

fn fallback_connection_id_path() -> Option<PathBuf> {
    dirs::data_local_dir().map(|d| d.join("fininsight-tally-agent").join(".connection_id"))
}

pub struct Vault;

impl Vault {
    fn entry_for(account: &str) -> Result<Entry, VaultError> {
        Entry::new(SERVICE_NAME, account).map_err(|e| VaultError::Keyring(e.to_string()))
    }

    fn entry() -> Result<Entry, VaultError> {
        Self::entry_for(TOKEN_ACCOUNT)
    }

    pub fn store_token(token: &str) -> Result<(), VaultError> {
        log::debug!("[vault] Storing token...");
        let mut keyring_ok = false;
        if let Ok(entry) = Self::entry() {
            if let Err(e) = entry.set_password(token) {
                log::debug!("[vault] Keyring set_password note: {:?}", e);
            } else {
                keyring_ok = true;
            }
        }
        if let Some(path) = fallback_token_path() {
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if let Err(e) = fs::write(&path, token) {
                log::debug!("[vault] Fallback write note: {:?}", e);
                if !keyring_ok {
                    return Err(VaultError::Keyring(e.to_string()));
                }
            }
        }
        log::debug!("[vault] Token stored successfully");
        Ok(())
    }

    pub fn get_token() -> Result<String, VaultError> {
        if let Ok(entry) = Self::entry() {
            if let Ok(token) = entry.get_password() {
                let trimmed = token.trim().to_string();
                if !trimmed.is_empty() {
                    return Ok(trimmed);
                }
            }
        }
        if let Some(path) = fallback_token_path() {
            if let Ok(token) = fs::read_to_string(&path) {
                let trimmed = token.trim().to_string();
                if !trimmed.is_empty() {
                    return Ok(trimmed);
                }
            }
        }
        Err(VaultError::NotFound)
    }

    pub fn delete_token() -> Result<(), VaultError> {
        if let Ok(entry) = Self::entry() {
            let _ = entry.delete_credential();
        }
        if let Some(path) = fallback_token_path() {
            let _ = fs::remove_file(&path);
        }
        Ok(())
    }

    pub fn store_connection_id(connection_id: &str) -> Result<(), VaultError> {
        log::debug!("[vault] Storing connection_id...");
        let mut keyring_ok = false;
        if let Ok(entry) = Self::entry_for(CONNECTION_ID_ACCOUNT) {
            if let Err(e) = entry.set_password(connection_id) {
                log::debug!("[vault] Keyring set_password connection_id note: {:?}", e);
            } else {
                keyring_ok = true;
            }
        }
        if let Some(path) = fallback_connection_id_path() {
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if let Err(e) = fs::write(&path, connection_id) {
                log::debug!("[vault] Fallback connection_id write note: {:?}", e);
                if !keyring_ok {
                    return Err(VaultError::Keyring(e.to_string()));
                }
            }
        }
        log::debug!("[vault] Connection ID stored successfully");
        Ok(())
    }

    pub fn get_connection_id() -> Result<String, VaultError> {
        if let Ok(entry) = Self::entry_for(CONNECTION_ID_ACCOUNT) {
            if let Ok(cid) = entry.get_password() {
                let trimmed = cid.trim().to_string();
                if !trimmed.is_empty() {
                    return Ok(trimmed);
                }
            }
        }
        if let Some(path) = fallback_connection_id_path() {
            if let Ok(cid) = fs::read_to_string(&path) {
                let trimmed = cid.trim().to_string();
                if !trimmed.is_empty() {
                    return Ok(trimmed);
                }
            }
        }
        Err(VaultError::NotFound)
    }

    pub fn delete_connection_id() -> Result<(), VaultError> {
        if let Ok(entry) = Self::entry_for(CONNECTION_ID_ACCOUNT) {
            let _ = entry.delete_credential();
        }
        if let Some(path) = fallback_connection_id_path() {
            let _ = fs::remove_file(&path);
        }
        Ok(())
    }

    pub fn is_paired() -> bool {
        Self::get_token().is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_roundtrip() {
        let account = format!("agent-token-test-{}", std::process::id());
        let entry = Vault::entry_for(&account).expect("entry");
        let _ = entry.delete_credential();
        let dummy = "test-agent-token-abc123xyz";
        entry.set_password(dummy).expect("store");
        let retrieved = entry.get_password().expect("retrieve");
        assert_eq!(retrieved, dummy);
        entry.delete_credential().expect("cleanup");
    }

    #[test]
    fn connection_id_vault_methods() {
        let dummy_cid = format!("conn-test-{}", std::process::id());
        Vault::store_connection_id(&dummy_cid).expect("store connection_id");
        let retrieved = Vault::get_connection_id().expect("get connection_id");
        assert_eq!(retrieved, dummy_cid);
        Vault::delete_connection_id().expect("delete connection_id");
        assert!(Vault::get_connection_id().is_err());
    }
}
