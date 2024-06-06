//! Cache a lot of JSON-(de)serializable items on disk.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::prelude::*;
use std::io::SeekFrom;
use std::marker::PhantomData;
use std::sync::Mutex;
use std::{collections::HashMap, sync::Arc};
use tempfile::tempfile;

#[derive(Clone, Debug)]
pub struct FileHash<KeyType, ValueType> {
    id2pos: HashMap<KeyType, (u64, u64)>, // Position, length
    file_handle: Option<Arc<Mutex<File>>>,
    last_action_was_read: Arc<Mutex<bool>>,
}

impl<KeyType: PartialEq + Eq + std::hash::Hash, ValueType: Serialize + for<'a> Deserialize<'a>>
    FileHash<KeyType, ValueType>
{
    pub fn new() -> Self {
        Self {
            id2pos: HashMap::new(),
            file_handle: None,
            last_action_was_read: Arc::new(Mutex::new(true)),
        }
    }

    pub fn len(&self) -> usize {
        self.id2pos.len()
    }

    pub fn is_empty(&self) -> bool {
        self.id2pos.is_empty()
    }

    /// Adds an entity for the key.
    /// Note that this will overwrite any existing entity for the key.
    /// Also, this will add the new value at the end of the file, wasting space for the previous one.
    pub fn insert<K: Into<KeyType>, V: Into<ValueType>>(&mut self, key: K, value: V) -> Result<()> {
        let fh = self.get_or_create_file_handle();
        let mut fh = fh.lock().map_err(|e| anyhow!(format!("{e}")))?;
        let before = fh.metadata()?.len();
        // Writes occur only at the end of the file, so only seek if the last action was "read"
        if *self
            .last_action_was_read
            .lock()
            .map_err(|e| anyhow!(format!("{e}")))?
        {
            // TODO is the new string fits into the old one, we could overwrite it to save disk space?
            fh.seek(SeekFrom::End(0))?;
        }
        *self
            .last_action_was_read
            .lock()
            .map_err(|e| anyhow!(format!("{e}")))? = false;
        let json = serde_json::to_string(&value.into())?;
        fh.write_all(json.as_bytes())?;
        let after = fh.metadata()?.len();
        let diff = after - before;
        self.id2pos.insert(key.into(), (before, diff));
        Ok(())
    }

    pub fn contains<K: Into<KeyType>>(&self, key: K) -> bool {
        self.id2pos.contains_key(&key.into())
    }

    pub fn get<K: Into<KeyType>>(&self, key: K) -> Option<ValueType> {
        let mut fh = self.file_handle.as_ref()?.lock().ok()?;
        *self.last_action_was_read.lock().ok()? = true;
        let (start, length) = self.id2pos.get(&key.into())?;
        fh.seek(SeekFrom::Start(*start)).ok()?;
        let mut buffer: Vec<u8> = vec![0; *length as usize];
        fh.read_exact(&mut buffer).ok()?;
        let s: String = String::from_utf8(buffer).ok()?;
        serde_json::from_str(&s).ok()
    }

    fn get_or_create_file_handle(&mut self) -> Arc<Mutex<File>> {
        if let Some(fh) = &self.file_handle {
            return fh.clone();
        }
        let fh = tempfile()
            .expect("FileHash::get_or_create_file_handle: Could not create temporary file");
        self.file_handle = Some(Arc::new(Mutex::new(fh))); // Should auto-destruct
        if let Some(fh) = &self.file_handle {
            return fh.clone();
        }
        panic!("FileHash::get_or_create_file_handle: This is weird");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_entity_file_cache() {
        let mut efc: FileHash<String, String> = FileHash::new();
        efc.insert("Q123", "Foo").unwrap();
        efc.insert("Q456", "Bar").unwrap();
        efc.insert("Q789", "Baz").unwrap();
        efc.insert("Q456", "Boom").unwrap();
        assert_eq!(efc.get("Q123").unwrap(), "Foo");
        assert_eq!(efc.get("Q456").unwrap(), "Boom");
        assert_eq!(efc.get("Q789").unwrap(), "Baz");
        assert_eq!(efc.get("Nope"), None);
    }
}
