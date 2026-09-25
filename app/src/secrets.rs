//! The Immich API key, kept in the GNOME keyring through the Secret Service. It is never written
//! to `app.db`, never logged and never shown again once it is kept.

use photomanager_core::APP_ID;

const PURPOSE: &str = "immich-api-key";
const LABEL: &str = "Photo Manager: Immich API key";

fn attributes() -> [(&'static str, &'static str); 2] {
    [("application", APP_ID), ("purpose", PURPOSE)]
}

#[cfg(feature = "devtools")]
thread_local! {
    static SESSION: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// A key for this run only, for a session without a keyring. Development builds only.
#[cfg(feature = "devtools")]
pub fn use_for_this_run(key: &str) {
    SESSION.with(|session| *session.borrow_mut() = Some(key.to_string()));
}

/// A keyring that is there answers at once; one that is not would keep the page waiting for the
/// bus to give up.
const REACH: std::time::Duration = std::time::Duration::from_secs(5);

async fn keyring() -> Result<oo7::Keyring, String> {
    let keyring = gtk::glib::future_with_timeout(REACH, oo7::Keyring::new())
        .await
        .map_err(|_| "no keyring answers, so no key can be kept".to_string())?
        .map_err(|error| format!("the keyring cannot be reached: {error}"))?;
    keyring
        .unlock()
        .await
        .map_err(|error| format!("the keyring stayed locked: {error}"))?;
    Ok(keyring)
}

/// The kept key, if there is one.
pub async fn immich_key() -> Result<Option<String>, String> {
    #[cfg(feature = "devtools")]
    if let Some(key) = SESSION.with(|session| session.borrow().clone()) {
        return Ok(Some(key));
    }
    let keyring = keyring().await?;
    let items = keyring
        .search_items(&attributes())
        .await
        .map_err(|error| format!("the keyring cannot be searched: {error}"))?;
    let Some(item) = items.first() else {
        return Ok(None);
    };
    let secret = item
        .secret()
        .await
        .map_err(|error| format!("the key cannot be read: {error}"))?;
    let key = String::from_utf8(secret.as_bytes().to_vec()).map_err(|_| "the kept key is not text".to_string())?;
    Ok(Some(key).filter(|key| !key.trim().is_empty()))
}

/// Keeps the key, replacing the one kept before.
pub async fn keep_immich_key(key: &str) -> Result<(), String> {
    let keyring = keyring().await?;
    keyring
        .create_item(LABEL, &attributes(), key.trim().as_bytes(), true)
        .await
        .map_err(|error| format!("the key cannot be kept: {error}"))
}

pub async fn forget_immich_key() -> Result<(), String> {
    #[cfg(feature = "devtools")]
    SESSION.with(|session| session.borrow_mut().take());
    let keyring = keyring().await?;
    keyring
        .delete(&attributes())
        .await
        .map_err(|error| format!("the key cannot be forgotten: {error}"))
}
