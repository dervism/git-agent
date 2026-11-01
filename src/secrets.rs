//! API keys in the macOS keychain, so they are never written to a file.
//!
//! Each key is a generic password with the service "git-agent" and the
//! server's host as the account (plus the port, if the URL names one). Keys for
//! several providers can be saved side by side.

use anyhow::{Context, Result};
use reqwest::Url;

/// Whether this system has a keychain git-agent can save keys in.
pub const SUPPORTED: bool = cfg!(target_os = "macos");

const SERVICE: &str = "git-agent";

/// The keychain account of the server at `base_url`, e.g. "api.openai.com".
pub fn account(base_url: &str) -> Option<String> {
    let url = Url::parse(base_url).ok()?;
    let host = url.host_str()?;
    Some(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    })
}

/// The saved key for the server at `base_url`, if there is one.
pub fn get(base_url: &str) -> Result<Option<String>> {
    let Some(account) = account(base_url) else {
        return Ok(None);
    };
    platform::get(&account)
        .with_context(|| format!("could not read the API key for {account} from the keychain"))
}

pub fn set(base_url: &str, key: &str) -> Result<()> {
    let account = account(base_url).context("the base URL has no host")?;
    platform::set(&account, key)
        .with_context(|| format!("could not save the API key for {account} in the keychain"))
}

/// Removes the saved key for the server at `base_url`, if there is one.
pub fn delete(base_url: &str) -> Result<()> {
    let Some(account) = account(base_url) else {
        return Ok(());
    };
    platform::delete(&account)
        .with_context(|| format!("could not remove the API key for {account} from the keychain"))
}

#[cfg(target_os = "macos")]
mod platform {
    use super::SERVICE;
    use anyhow::Result;
    use security_framework::passwords::{
        PasswordOptions, delete_generic_password, generic_password, set_generic_password,
    };

    /// `errSecItemNotFound`
    const NOT_FOUND: i32 = -25300;

    pub fn get(account: &str) -> Result<Option<String>> {
        match generic_password(PasswordOptions::new_generic_password(SERVICE, account)) {
            Ok(key) => Ok(Some(String::from_utf8(key)?)),
            Err(err) if err.code() == NOT_FOUND => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    pub fn set(account: &str, key: &str) -> Result<()> {
        Ok(set_generic_password(SERVICE, account, key.as_bytes())?)
    }

    pub fn delete(account: &str) -> Result<()> {
        match delete_generic_password(SERVICE, account) {
            Err(err) if err.code() != NOT_FOUND => Err(err.into()),
            _ => Ok(()),
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use anyhow::{Result, bail};

    pub fn get(_account: &str) -> Result<Option<String>> {
        Ok(None)
    }

    pub fn set(_account: &str, _key: &str) -> Result<()> {
        bail!("git-agent saves API keys only on macOS; set GIT_AGENT_API_KEY instead")
    }

    pub fn delete(_account: &str) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accounts_are_hosts_with_explicit_ports() {
        assert_eq!(
            account("https://api.openai.com/v1").as_deref(),
            Some("api.openai.com")
        );
        assert_eq!(
            account("http://localhost:1234/v1").as_deref(),
            Some("localhost:1234")
        );
        assert_eq!(account("not a url"), None);
    }

    /// Writes to the real keychain, so it only runs when asked for:
    /// `cargo test -- --ignored keychain`
    #[test]
    #[ignore]
    #[cfg(target_os = "macos")]
    fn keychain_round_trip() {
        let base_url = "https://git-agent-test.invalid/v1";
        delete(base_url).unwrap();
        assert_eq!(get(base_url).unwrap(), None);
        set(base_url, "first").unwrap();
        set(base_url, "second").unwrap();
        assert_eq!(get(base_url).unwrap().as_deref(), Some("second"));
        delete(base_url).unwrap();
        assert_eq!(get(base_url).unwrap(), None);
    }
}
