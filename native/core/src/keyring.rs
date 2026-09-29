//! API keys in the desktop keyring, with `secrets.env` as the fallback.
//!
//! The keyring is the freedesktop Secret Service (GNOME Keyring, KWallet,
//! KeePassXC …) on the session D-Bus. A key moves there only when the user
//! asks (Settings › Accounts), and moves back the same way; the move writes
//! and reads back the new place before removing the old one, so a key is
//! never lost. `config/keyring.json` (mode 600) lists which keys live in the
//! keyring; everything else stays in `secrets.env`, which is what headless
//! machines, SSH sessions and `shadowcode serve` use.
//!
//! Keys are stored with the attributes `application=shadowcode`, `profile`
//! (a hash of the settings folder, so separate profiles never share keys)
//! and `name`. ShadowCode never unlocks the keyring on its own: a locked
//! keyring is reported, and the key is unavailable until it is unlocked.
use crate::paths::AppPaths;
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeSet, HashMap},
    sync::{mpsc, Mutex, OnceLock},
    time::Duration,
};

/// Longest wait for the keyring.
const TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Default, Serialize, Deserialize)]
struct Listed {
    #[serde(default)]
    names: BTreeSet<String>,
}

fn list_path(paths: &AppPaths) -> std::path::PathBuf {
    paths.config.join("keyring.json")
}

/// The keys kept in the keyring.
pub fn listed(paths: &AppPaths) -> BTreeSet<String> {
    std::fs::read(list_path(paths))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Listed>(&bytes).ok())
        .map(|l| l.names)
        .unwrap_or_default()
}

fn set_listed(paths: &AppPaths, names: BTreeSet<String>) -> Result<()> {
    if names.is_empty() {
        let _ = std::fs::remove_file(list_path(paths));
        return Ok(());
    }
    crate::paths::atomic_write(
        &list_path(paths),
        &serde_json::to_vec_pretty(&Listed { names })?,
        true,
    )
}

fn profile_id(paths: &AppPaths) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(paths.config.to_string_lossy().as_bytes());
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

fn cache() -> &'static Mutex<HashMap<String, String>> {
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    CACHE.get_or_init(Mutex::default)
}

fn cache_key(paths: &AppPaths, name: &str) -> String {
    format!("{}:{name}", profile_id(paths))
}

/// Where the keys go: the real Secret Service, or (debug builds only, for
/// tests) a JSON file named by `SHADOWCODE_TEST_KEYRING`.
trait Vault: Send {
    fn get(&self, attributes: &[(&str, &str)]) -> Result<Option<String>>;
    fn put(&self, attributes: &[(&str, &str)], label: &str, value: &str) -> Result<()>;
    fn delete(&self, attributes: &[(&str, &str)]) -> Result<()>;
}

fn vault() -> Result<Box<dyn Vault>> {
    #[cfg(debug_assertions)]
    if let Some(file) = std::env::var_os("SHADOWCODE_TEST_KEYRING") {
        return Ok(Box::new(fake::FileVault(file.into())));
    }
    Ok(Box::new(dbus::SecretService::open()?))
}

/// Run `work` against the keyring on its own thread, within [`TIMEOUT`].
fn with_vault<T: Send + 'static>(
    work: impl FnOnce(&dyn Vault) -> Result<T> + Send + 'static,
) -> Result<T> {
    let (sender, receiver) = mpsc::channel();
    std::thread::Builder::new()
        .name("shadowcode-keyring".into())
        .spawn(move || {
            let result = vault().and_then(|vault| work(vault.as_ref()));
            let _ = sender.send(result);
        })?;
    receiver
        .recv_timeout(TIMEOUT)
        .map_err(|_| anyhow::anyhow!("The keyring did not answer in time"))?
}

fn attributes(profile: &str, name: &str) -> Vec<(String, String)> {
    vec![
        ("application".into(), "shadowcode".into()),
        ("profile".into(), profile.into()),
        ("name".into(), name.into()),
    ]
}

fn borrowed(pairs: &[(String, String)]) -> Vec<(&str, &str)> {
    pairs
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect()
}

/// Whether a keyring answers here, and why not.
pub fn availability() -> std::result::Result<(), String> {
    #[cfg(debug_assertions)]
    if std::env::var_os("SHADOWCODE_TEST_KEYRING").is_some() {
        return Ok(());
    }
    if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none()
        && std::env::var_os("XDG_RUNTIME_DIR").is_none()
    {
        return Err("This session has no desktop keyring (no D-Bus session).".into());
    }
    with_vault(|_| Ok(())).map_err(|error| format!("{error:#}"))
}

/// A key from the keyring.
pub fn get(paths: &AppPaths, name: &str) -> Result<Option<String>> {
    let key = cache_key(paths, name);
    if let Some(value) = cache().lock().ok().and_then(|c| c.get(&key).cloned()) {
        return Ok(Some(value));
    }
    let pairs = attributes(&profile_id(paths), name);
    let value = with_vault(move |vault| vault.get(&borrowed(&pairs)))?;
    if let (Some(value), Ok(mut cache)) = (&value, cache().lock()) {
        cache.insert(key, value.clone());
    }
    Ok(value)
}

fn put(paths: &AppPaths, name: &str, value: &str) -> Result<()> {
    let pairs = attributes(&profile_id(paths), name);
    let label = format!("ShadowCode: {name}");
    let value_owned = value.to_owned();
    with_vault(move |vault| vault.put(&borrowed(&pairs), &label, &value_owned))?;
    if let Ok(mut cache) = cache().lock() {
        cache.insert(cache_key(paths, name), value.to_owned());
    }
    Ok(())
}

fn delete(paths: &AppPaths, name: &str) -> Result<()> {
    let pairs = attributes(&profile_id(paths), name);
    with_vault(move |vault| vault.delete(&borrowed(&pairs)))?;
    if let Ok(mut cache) = cache().lock() {
        cache.remove(&cache_key(paths, name));
    }
    Ok(())
}

/// Store a key that lives in the keyring (empty removes it).
pub fn set(paths: &AppPaths, name: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        delete(paths, name)?;
        let mut names = listed(paths);
        names.remove(name);
        set_listed(paths, names)
    } else {
        put(paths, name, value)
    }
}

/// Move a key from `secrets.env` into the keyring: write it, read it back,
/// and only then remove it from the file.
pub fn move_in(paths: &AppPaths, name: &str) -> Result<()> {
    ensure!(
        !listed(paths).contains(name),
        "This key is already in the keyring"
    );
    let value = crate::config::file_secret(paths, name)?
        .context("There is no saved key with this name to move")?;
    availability().map_err(|reason| anyhow::anyhow!(reason))?;
    put(paths, name, &value)?;
    // Read back from the keyring itself, not the cache.
    if let Ok(mut cache) = cache().lock() {
        cache.remove(&cache_key(paths, name));
    }
    ensure!(
        get(paths, name)?.as_deref() == Some(value.as_str()),
        "The keyring did not keep the key; it stays in secrets.env"
    );
    let mut names = listed(paths);
    names.insert(name.to_owned());
    set_listed(paths, names)?;
    crate::config::remove_file_secret(paths, name)
}

/// Move a key from the keyring back into `secrets.env`.
pub fn move_out(paths: &AppPaths, name: &str) -> Result<()> {
    ensure!(
        listed(paths).contains(name),
        "This key is not in the keyring"
    );
    let value = get(paths, name)?
        .context("The keyring doesn't have this key (it may be locked, or it was removed there)")?;
    crate::config::write_file_secret(paths, name, &value)?;
    ensure!(
        crate::config::file_secret(paths, name)?.as_deref() == Some(value.as_str()),
        "Could not write the key to secrets.env; it stays in the keyring"
    );
    let mut names = listed(paths);
    names.remove(name);
    set_listed(paths, names)?;
    if let Err(error) = delete(paths, name) {
        tracing::warn!("keyring.delete_after_move error={error:#}");
    }
    Ok(())
}

mod dbus {
    //! The freedesktop Secret Service, spoken directly over D-Bus with the
    //! `plain` session algorithm (the session bus is private to the user).
    use super::Vault;
    use anyhow::{bail, Context, Result};
    use std::collections::HashMap;
    use zbus::{
        blocking::Connection,
        zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value},
    };

    const DEST: &str = "org.freedesktop.secrets";
    const PATH: &str = "/org/freedesktop/secrets";
    const SERVICE: &str = "org.freedesktop.Secret.Service";
    type Secret = (OwnedObjectPath, Vec<u8>, Vec<u8>, String);

    pub struct SecretService {
        connection: Connection,
        session: OwnedObjectPath,
    }

    impl SecretService {
        pub fn open() -> Result<Self> {
            let connection =
                Connection::session().context("Could not reach the desktop session bus")?;
            let reply = connection
                .call_method(
                    Some(DEST),
                    PATH,
                    Some(SERVICE),
                    "OpenSession",
                    &("plain", Value::from("")),
                )
                .context("No keyring service is running")?;
            let (_, session): (OwnedValue, OwnedObjectPath) = reply.body().deserialize()?;
            Ok(Self {
                connection,
                session,
            })
        }
        fn search(
            &self,
            attributes: &[(&str, &str)],
        ) -> Result<(Vec<OwnedObjectPath>, Vec<OwnedObjectPath>)> {
            let map: HashMap<&str, &str> = attributes.iter().copied().collect();
            let reply = self.connection.call_method(
                Some(DEST),
                PATH,
                Some(SERVICE),
                "SearchItems",
                &(map,),
            )?;
            Ok(reply.body().deserialize()?)
        }
        fn default_collection(&self) -> Result<OwnedObjectPath> {
            let reply = self.connection.call_method(
                Some(DEST),
                PATH,
                Some(SERVICE),
                "ReadAlias",
                &("default",),
            )?;
            let path: OwnedObjectPath = reply.body().deserialize()?;
            if path.as_str() == "/" {
                bail!("The keyring has no default collection");
            }
            Ok(path)
        }
    }

    impl Drop for SecretService {
        fn drop(&mut self) {
            let _ = self.connection.call_method(
                Some(DEST),
                self.session.as_str(),
                Some("org.freedesktop.Secret.Session"),
                "Close",
                &(),
            );
        }
    }

    impl Vault for SecretService {
        fn get(&self, attributes: &[(&str, &str)]) -> Result<Option<String>> {
            let (unlocked, locked) = self.search(attributes)?;
            let Some(item) = unlocked.first() else {
                if !locked.is_empty() {
                    bail!("The keyring is locked; unlock it to use this key");
                }
                return Ok(None);
            };
            let reply = self.connection.call_method(
                Some(DEST),
                PATH,
                Some(SERVICE),
                "GetSecrets",
                &(vec![item.clone()], self.session.clone()),
            )?;
            let secrets: HashMap<OwnedObjectPath, Secret> = reply.body().deserialize()?;
            let Some((_, _, value, _)) = secrets.into_values().next() else {
                return Ok(None);
            };
            Ok(Some(
                String::from_utf8(value).context("The stored key is not text")?,
            ))
        }
        fn put(&self, attributes: &[(&str, &str)], label: &str, value: &str) -> Result<()> {
            let collection = self.default_collection()?;
            let map: HashMap<&str, &str> = attributes.iter().copied().collect();
            let mut properties: HashMap<&str, Value> = HashMap::new();
            properties.insert("org.freedesktop.Secret.Item.Label", Value::from(label));
            properties.insert("org.freedesktop.Secret.Item.Attributes", Value::from(map));
            let secret = (
                self.session.clone(),
                Vec::<u8>::new(),
                value.as_bytes().to_vec(),
                "text/plain",
            );
            let reply = self.connection.call_method(
                Some(DEST),
                collection.as_str(),
                Some("org.freedesktop.Secret.Collection"),
                "CreateItem",
                &(properties, secret, true),
            )?;
            let (_, prompt): (OwnedObjectPath, OwnedObjectPath) = reply.body().deserialize()?;
            if prompt.as_str() != "/" {
                bail!("The keyring is locked; unlock it, then try again");
            }
            Ok(())
        }
        fn delete(&self, attributes: &[(&str, &str)]) -> Result<()> {
            let (unlocked, locked) = self.search(attributes)?;
            if !locked.is_empty() {
                bail!("The keyring is locked; unlock it, then try again");
            }
            for item in unlocked {
                let path: ObjectPath = item.as_ref();
                let reply = self.connection.call_method(
                    Some(DEST),
                    path,
                    Some("org.freedesktop.Secret.Item"),
                    "Delete",
                    &(),
                )?;
                let prompt: OwnedObjectPath = reply.body().deserialize()?;
                if prompt.as_str() != "/" {
                    bail!("The keyring asked for confirmation; remove the key there");
                }
            }
            Ok(())
        }
    }
}

#[cfg(debug_assertions)]
mod fake {
    //! A stand-in keyring for tests: a JSON file of attribute sets.
    use super::Vault;
    use anyhow::Result;
    use std::path::PathBuf;

    pub struct FileVault(pub PathBuf);
    impl FileVault {
        fn read(&self) -> Vec<(Vec<(String, String)>, String)> {
            std::fs::read(&self.0)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default()
        }
        fn write(&self, items: &[(Vec<(String, String)>, String)]) -> Result<()> {
            std::fs::write(&self.0, serde_json::to_vec(items)?)?;
            Ok(())
        }
    }
    fn same(a: &[(String, String)], b: &[(&str, &str)]) -> bool {
        a.len() == b.len()
            && b.iter()
                .all(|(k, v)| a.iter().any(|(x, y)| x == k && y == v))
    }
    impl Vault for FileVault {
        fn get(&self, attributes: &[(&str, &str)]) -> Result<Option<String>> {
            Ok(self
                .read()
                .into_iter()
                .find(|(a, _)| same(a, attributes))
                .map(|(_, v)| v))
        }
        fn put(&self, attributes: &[(&str, &str)], _label: &str, value: &str) -> Result<()> {
            let mut items = self.read();
            items.retain(|(a, _)| !same(a, attributes));
            items.push((
                attributes
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                    .collect(),
                value.to_owned(),
            ));
            self.write(&items)
        }
        fn delete(&self, attributes: &[(&str, &str)]) -> Result<()> {
            let mut items = self.read();
            items.retain(|(a, _)| !same(a, attributes));
            self.write(&items)
        }
    }
}

/// Where each saved key lives, for Settings: `{keyring: {available,
/// detail}, keys: [{name, place: "file"|"keyring"}]}`. Values are never
/// included.
pub fn overview(paths: &AppPaths) -> Result<serde_json::Value> {
    let in_keyring = listed(paths);
    let mut keys: Vec<serde_json::Value> = crate::config::file_secret_names(paths)?
        .into_iter()
        .filter(|name| !in_keyring.contains(name))
        .map(|name| serde_json::json!({"name":name,"place":"file"}))
        .collect();
    keys.extend(
        in_keyring
            .iter()
            .map(|name| serde_json::json!({"name":name,"place":"keyring"})),
    );
    let available = availability();
    Ok(serde_json::json!({
        "keyring": {
            "available": available.is_ok(),
            "detail": available.err(),
        },
        "file": paths.secrets_file(),
        "keys": keys,
    }))
}

/// Refuse names that are not keys ShadowCode stores.
pub fn check_name(name: &str) -> Result<()> {
    if !crate::config::valid_secret_name(name) {
        bail!("Invalid key name");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test: `SHADOWCODE_TEST_KEYRING` is process-wide.
    #[test]
    fn keys_move_into_the_keyring_and_back_without_being_lost() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SHADOWCODE_TEST_KEYRING", dir.path().join("vault.json"));
        let paths = AppPaths::isolated(&dir.path().join("profile")).unwrap();
        let other = AppPaths::isolated(&dir.path().join("other")).unwrap();
        crate::config::set_secret(&paths, "OPENROUTER_API_KEY", "sk-or-placeholder-value").unwrap();
        crate::config::set_secret(&other, "OPENROUTER_API_KEY", "other-profile-value").unwrap();
        assert!(
            move_out(&paths, "OPENROUTER_API_KEY").is_err(),
            "not in the keyring yet"
        );

        move_in(&paths, "OPENROUTER_API_KEY").unwrap();
        assert!(listed(&paths).contains("OPENROUTER_API_KEY"));
        assert_eq!(
            crate::config::file_secret(&paths, "OPENROUTER_API_KEY").unwrap(),
            None
        );
        // Reads go to the keyring; other profiles keep their own key.
        assert_eq!(
            crate::config::secret(&paths, "OPENROUTER_API_KEY")
                .unwrap()
                .as_deref(),
            Some("sk-or-placeholder-value")
        );
        move_in(&other, "OPENROUTER_API_KEY").unwrap();
        cache().lock().unwrap().clear();
        assert_eq!(
            crate::config::secret(&other, "OPENROUTER_API_KEY")
                .unwrap()
                .as_deref(),
            Some("other-profile-value")
        );
        assert_eq!(
            crate::config::secret(&paths, "OPENROUTER_API_KEY")
                .unwrap()
                .as_deref(),
            Some("sk-or-placeholder-value")
        );
        // Saving a new key while it lives in the keyring stays there.
        crate::config::set_secret(&paths, "OPENROUTER_API_KEY", "sk-or-second-value").unwrap();
        cache().lock().unwrap().clear();
        assert_eq!(
            get(&paths, "OPENROUTER_API_KEY").unwrap().as_deref(),
            Some("sk-or-second-value")
        );
        assert_eq!(
            crate::config::file_secret(&paths, "OPENROUTER_API_KEY").unwrap(),
            None
        );
        let overview = overview(&paths).unwrap();
        assert_eq!(overview["keys"][0]["place"], "keyring");
        assert!(!overview.to_string().contains("sk-or-second-value"));

        move_out(&paths, "OPENROUTER_API_KEY").unwrap();
        assert!(listed(&paths).is_empty());
        assert_eq!(
            crate::config::file_secret(&paths, "OPENROUTER_API_KEY")
                .unwrap()
                .as_deref(),
            Some("sk-or-second-value")
        );
        cache().lock().unwrap().clear();
        assert_eq!(
            get(&paths, "OPENROUTER_API_KEY").unwrap(),
            None,
            "removed from the keyring"
        );
        // Removing a key that lives in the keyring removes it everywhere.
        crate::config::set_secret(&other, "OPENROUTER_API_KEY", "").unwrap();
        assert!(listed(&other).is_empty());
        cache().lock().unwrap().clear();
        assert_eq!(
            crate::config::secret(&other, "OPENROUTER_API_KEY").unwrap(),
            None
        );
        std::env::remove_var("SHADOWCODE_TEST_KEYRING");
    }

    /// The real Secret Service, in a private session made by the caller:
    /// `dbus-run-session` with an unlocked `gnome-keyring-daemon` and a
    /// temporary XDG_DATA_HOME (see docs/OPENROUTER.md).
    #[test]
    #[ignore = "needs a private keyring session"]
    fn live_secret_service_round_trip() {
        assert!(std::env::var_os("SHADOWCODE_TEST_KEYRING").is_none());
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::isolated(&dir.path().join("profile")).unwrap();
        availability().unwrap();
        crate::config::set_secret(&paths, "SHADOWCODE_LIVE_TEST", "live-placeholder").unwrap();
        move_in(&paths, "SHADOWCODE_LIVE_TEST").unwrap();
        cache().lock().unwrap().clear();
        assert_eq!(
            get(&paths, "SHADOWCODE_LIVE_TEST").unwrap().as_deref(),
            Some("live-placeholder")
        );
        move_out(&paths, "SHADOWCODE_LIVE_TEST").unwrap();
        cache().lock().unwrap().clear();
        assert_eq!(get(&paths, "SHADOWCODE_LIVE_TEST").unwrap(), None);
    }
}
