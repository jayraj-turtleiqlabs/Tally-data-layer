use keyring::Entry;

use crate::errors::VaultError;

const SERVICE_NAME: &str = "com.fininsight.tally-agent";
const LEGACY_TOKEN_ACCOUNT: &str = "agent-token";
const CONNECTION_ID_ACCOUNT: &str = "connection-id";

pub struct Vault;

impl Vault {
    fn entry_for(account: &str) -> Result<Entry, VaultError> {
        Entry::new(SERVICE_NAME, account).map_err(|e| VaultError::Keyring(e.to_string()))
    }

    fn token_account_for(connection_id: &str) -> String {
        format!("agent-token:{}", connection_id)
    }

    pub fn store_token_for(connection_id: &str, token: &str) -> Result<(), VaultError> {
        let account = Self::token_account_for(connection_id);
        let entry = Self::entry_for(&account)?;
        entry.set_password(token).map_err(|e| VaultError::Keyring(e.to_string()))?;
        // Clean up legacy single token if present
        if let Ok(legacy) = Self::entry_for(LEGACY_TOKEN_ACCOUNT) {
            let _ = legacy.delete_credential();
        }
        Ok(())
    }

    pub fn get_token_for(connection_id: &str) -> Result<String, VaultError> {
        let account = Self::token_account_for(connection_id);
        let entry = Self::entry_for(&account)?;
        match entry.get_password() {
            Ok(token) => {
                let trimmed = token.trim().to_string();
                if !trimmed.is_empty() {
                    return Ok(trimmed);
                }
            }
            Err(keyring::Error::NoEntry) => {}
            Err(e) => return Err(VaultError::Keyring(e.to_string())),
        }

        Err(VaultError::NotFound)
    }

    pub fn delete_token_for(connection_id: &str) -> Result<(), VaultError> {
        let account = Self::token_account_for(connection_id);
        if let Ok(entry) = Self::entry_for(&account) {
            let _ = entry.delete_credential();
        }
        if let Ok(legacy) = Self::entry_for(LEGACY_TOKEN_ACCOUNT) {
            let _ = legacy.delete_credential();
        }
        Ok(())
    }

    pub fn store_token(token: &str) -> Result<(), VaultError> {
        let entry = Self::entry_for(LEGACY_TOKEN_ACCOUNT)?;
        entry.set_password(token).map_err(|e| VaultError::Keyring(e.to_string()))
    }

    pub fn get_token() -> Result<String, VaultError> {
        let entry = Self::entry_for(LEGACY_TOKEN_ACCOUNT)?;
        match entry.get_password() {
            Ok(token) => {
                let trimmed = token.trim().to_string();
                if !trimmed.is_empty() {
                    Ok(trimmed)
                } else {
                    Err(VaultError::NotFound)
                }
            }
            Err(keyring::Error::NoEntry) => Err(VaultError::NotFound),
            Err(e) => Err(VaultError::Keyring(e.to_string())),
        }
    }

    pub fn delete_token() -> Result<(), VaultError> {
        if let Ok(entry) = Self::entry_for(LEGACY_TOKEN_ACCOUNT) {
            let _ = entry.delete_credential();
        }
        Ok(())
    }

    pub fn store_connection_id(connection_id: &str) -> Result<(), VaultError> {
        let entry = Self::entry_for(CONNECTION_ID_ACCOUNT)?;
        entry.set_password(connection_id).map_err(|e| VaultError::Keyring(e.to_string()))
    }

    pub fn get_connection_id() -> Result<String, VaultError> {
        let entry = Self::entry_for(CONNECTION_ID_ACCOUNT)?;
        match entry.get_password() {
            Ok(cid) => {
                let trimmed = cid.trim().to_string();
                if !trimmed.is_empty() {
                    Ok(trimmed)
                } else {
                    Err(VaultError::NotFound)
                }
            }
            Err(keyring::Error::NoEntry) => Err(VaultError::NotFound),
            Err(e) => Err(VaultError::Keyring(e.to_string())),
        }
    }

    pub fn delete_connection_id() -> Result<(), VaultError> {
        if let Ok(entry) = Self::entry_for(CONNECTION_ID_ACCOUNT) {
            let _ = entry.delete_credential();
        }
        Ok(())
    }

    pub fn is_paired() -> bool {
        if let Ok(cid) = Self::get_connection_id() {
            Self::get_token_for(&cid).is_ok()
        } else {
            Self::get_token().is_ok()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn token_roundtrip() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let account = format!("agent-token-test-{}", std::process::id());
        let entry1 = Vault::entry_for(&account).expect("entry1");
        let _ = entry1.delete_credential();
        let dummy = "test-agent-token-abc123xyz";
        entry1.set_password(dummy).expect("store");
        let entry2 = Vault::entry_for(&account).expect("entry2");
        let retrieved = entry2.get_password().expect("retrieve");
        assert_eq!(retrieved, dummy);
        entry2.delete_credential().expect("cleanup");
    }

    #[test]
    fn connection_id_vault_methods() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let account = format!("connection-id-test-{}", std::process::id());
        let entry1 = Vault::entry_for(&account).expect("entry1");
        let _ = entry1.delete_credential();
        let dummy = "conn-test-12345";
        entry1.set_password(dummy).expect("store");
        let entry2 = Vault::entry_for(&account).expect("entry2");
        let retrieved = entry2.get_password().expect("retrieve");
        assert_eq!(retrieved, dummy);
        entry2.delete_credential().expect("cleanup");
    }
}
