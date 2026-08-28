//! OS credential vault wrapper — agent token never touches plaintext files.

use keyring::Entry;

use crate::errors::VaultError;

const SERVICE_NAME: &str = "com.fininsight.tally-agent";
const TOKEN_ACCOUNT: &str = "agent-token";

pub struct Vault;

impl Vault {
    fn entry_for(account: &str) -> Result<Entry, VaultError> {
        Entry::new(SERVICE_NAME, account).map_err(|e| VaultError::Keyring(e.to_string()))
    }

    fn entry() -> Result<Entry, VaultError> {
        Self::entry_for(TOKEN_ACCOUNT)
    }

    pub fn store_token(token: &str) -> Result<(), VaultError> {
        Self::entry()?
            .set_password(token)
            .map_err(|e| VaultError::Keyring(e.to_string()))
    }

    pub fn get_token() -> Result<String, VaultError> {
        Self::entry()?
            .get_password()
            .map_err(|e| match e {
                keyring::Error::NoEntry => VaultError::NotFound,
                other => {
                    if other.to_string().contains("No matching entry") {
                        VaultError::NotFound
                    } else {
                        VaultError::Keyring(other.to_string())
                    }
                }
            })
    }

    pub fn delete_token() -> Result<(), VaultError> {
        match Self::entry()?.delete_credential() {
            Ok(()) => Ok(()),
            Err(e) if e.to_string().contains("No matching entry") => Ok(()),
            Err(e) => Err(VaultError::Keyring(e.to_string())),
        }
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
}
