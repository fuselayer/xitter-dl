//! The classifier's API key, kept out of the app's own files.
//!
//! ## Why not `settings.json`
//!
//! A key in a JSON file beside the library is a key in every backup, every
//! sync folder and every bug report that includes the data directory. Windows
//! already has the right place for a secret — Credential Manager, per user,
//! unlocked with the login — so the app uses it and stores only the provider id
//! and model in its own file.
//!
//! ## What the webview is allowed to know
//!
//! That a key exists, and its last four characters. Never the key: the field is
//! write-only from the UI's point of view, which is why the settings panel can
//! show "saved" but cannot reveal what was saved. If the key needs to be seen
//! again, it has to be re-entered and re-issued by the provider.

use keyring::Entry;

const SERVICE: &str = "xitter-dl";

fn entry(provider: &str) -> Result<Entry, String> {
    Entry::new(SERVICE, provider).map_err(|e| format!("the credential store is unavailable: {e}"))
}

/// The stored key for a provider, if there is one.
pub fn get(provider: &str) -> Option<String> {
    let entry = entry(provider).ok()?;
    entry.get_password().ok().filter(|k| !k.trim().is_empty())
}

/// Store a key, replacing whatever was there.
pub fn set(provider: &str, key: &str) -> Result<(), String> {
    entry(provider)?
        .set_password(key)
        .map_err(|e| format!("could not save the key: {e}"))
}

/// Forget a key. Missing is not an error: the goal is "no key", and it is met.
pub fn clear(provider: &str) -> Result<(), String> {
    match entry(provider)?.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("could not remove the key: {e}")),
    }
}

/// Enough of a key to recognise it, never enough to use it.
pub fn hint(key: &str) -> String {
    let tail: String = {
        let chars: Vec<char> = key.chars().collect();
        let start = chars.len().saturating_sub(4);
        chars[start..].iter().collect()
    };
    format!("••••{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hint_keeps_four_characters_and_nothing_else() {
        assert_eq!(hint("sk-abcdefghijklmnop-1234"), "••••1234");
        assert_eq!(hint("abc"), "••••abc");
        assert_eq!(hint(""), "••••");
    }
}
