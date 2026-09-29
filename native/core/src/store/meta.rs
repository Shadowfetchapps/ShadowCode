//! `native_meta`: a small key/value table (keys in `keys.rs`) that also
//! holds JSON documents. A document that is read, changed and written back
//! goes through one `BEGIN IMMEDIATE` transaction, so two writers — another
//! task in this process or another ShadowCode process on the same profile —
//! can never both read the old value and lose one update.
use super::Store;
use anyhow::Result;
use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};
use serde::{de::DeserializeOwned, Serialize};

impl Store {
    pub fn native_meta(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .lock()?
            .query_row("SELECT value FROM native_meta WHERE key=?", [key], |r| {
                r.get::<_, String>(0)
            })
            .optional()?)
    }
    pub fn set_native_meta(&self, key: &str, value: &str) -> Result<()> {
        self.lock()?.execute(
            "INSERT INTO native_meta(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
        Ok(())
    }
    /// Run `work` in one write transaction over `native_meta`. Returning an
    /// error rolls every write back.
    pub fn meta_transaction<R>(
        &self,
        work: impl FnOnce(&MetaTransaction<'_>) -> Result<R>,
    ) -> Result<R> {
        let mut db = self.lock()?;
        // IMMEDIATE takes the write lock before the first read, so a second
        // process waits (busy_timeout) instead of reading a value this
        // transaction is about to replace.
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = work(&MetaTransaction { tx: &tx })?;
        tx.commit()?;
        Ok(result)
    }
    /// Read-modify-write one JSON document atomically. A missing or
    /// unreadable document starts from `T::default()`.
    pub fn update_native_json<T, R>(&self, key: &str, update: impl FnOnce(&mut T) -> R) -> Result<R>
    where
        T: DeserializeOwned + Serialize + Default,
    {
        self.meta_transaction(|meta| {
            let mut value: T = meta
                .get(key)?
                .and_then(|text| serde_json::from_str(&text).ok())
                .unwrap_or_default();
            let result = update(&mut value);
            meta.set_json(key, &value)?;
            Ok(result)
        })
    }
}

/// `native_meta` inside one [`Store::meta_transaction`].
pub struct MetaTransaction<'a> {
    tx: &'a Transaction<'a>,
}

impl MetaTransaction<'_> {
    pub fn get(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .tx
            .query_row("SELECT value FROM native_meta WHERE key=?", [key], |r| {
                r.get::<_, String>(0)
            })
            .optional()?)
    }
    pub fn set(&self, key: &str, value: &str) -> Result<()> {
        self.tx.execute(
            "INSERT INTO native_meta(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
        Ok(())
    }
    pub fn delete(&self, key: &str) -> Result<()> {
        self.tx
            .execute("DELETE FROM native_meta WHERE key=?", [key])?;
        Ok(())
    }
    /// A JSON document; a document that does not parse as `T` is an error.
    pub fn json<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        self.get(key)?
            .map(|text| serde_json::from_str(&text).map_err(Into::into))
            .transpose()
    }
    pub fn set_json<T: Serialize + ?Sized>(&self, key: &str, value: &T) -> Result<()> {
        self.set(key, &serde_json::to_string(value)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    /// Writers on separate connections (as two ShadowCode processes would
    /// have) and on one shared connection increment the same document at
    /// the same time; every increment survives.
    #[test]
    fn concurrent_json_updates_lose_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("native.sqlite");
        let shared = Arc::new(Store::open(&path).unwrap());
        let stores: Vec<Arc<Store>> = (0..4)
            .map(|i| {
                if i % 2 == 0 {
                    shared.clone()
                } else {
                    Arc::new(Store::open(&path).unwrap())
                }
            })
            .collect();
        const ROUNDS: u64 = 50;
        let barrier = Arc::new(Barrier::new(stores.len()));
        let workers: Vec<_> = stores
            .into_iter()
            .enumerate()
            .map(|(worker, store)| {
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    for round in 0..ROUNDS {
                        store
                            .update_native_json("counter", |count: &mut u64| *count += 1)
                            .unwrap();
                        store
                            .update_native_json("log", |ids: &mut Vec<String>| {
                                ids.push(format!("{worker}:{round}"))
                            })
                            .unwrap();
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        let count: u64 =
            serde_json::from_str(&shared.native_meta("counter").unwrap().unwrap()).unwrap();
        assert_eq!(count, 4 * ROUNDS);
        let log: Vec<String> =
            serde_json::from_str(&shared.native_meta("log").unwrap().unwrap()).unwrap();
        assert_eq!(log.len() as u64, 4 * ROUNDS);
    }

    #[test]
    fn failed_transactions_write_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("native.sqlite")).unwrap();
        store.set_native_meta("a", "1").unwrap();
        let result: Result<()> = store.meta_transaction(|meta| {
            meta.set("a", "2")?;
            meta.set("b", "2")?;
            anyhow::bail!("stop")
        });
        assert!(result.is_err());
        assert_eq!(store.native_meta("a").unwrap().as_deref(), Some("1"));
        assert_eq!(store.native_meta("b").unwrap(), None);
        // Unreadable documents restart from the default.
        store.set_native_meta("list", "not json").unwrap();
        let len = store
            .update_native_json("list", |ids: &mut Vec<String>| {
                ids.push("x".into());
                ids.len()
            })
            .unwrap();
        assert_eq!(len, 1);
    }
}
