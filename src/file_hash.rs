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

pub type EntityFileCache = FileHash<String, String>;

/// Maximum number of rows to keep in memory before flushing to disk.
/// This is a default value that can be overridden on the individual tables.
const MAX_MEM_ENTRIES: usize = 5;

#[derive(Clone, Debug)]
pub struct FileHash<KeyType, ValueType> {
    id2pos: HashMap<KeyType, (u64, u64)>, // Position, length
    file_handle: Option<Arc<Mutex<File>>>,
    phantom: PhantomData<ValueType>,
    in_memory: HashMap<KeyType, ValueType>,
    disk_free: Vec<(u64, u64)>,
    max_mem_entries: usize,
    using_disk: bool,
}

impl<
        KeyType: Clone + PartialEq + Eq + std::hash::Hash,
        ValueType: Clone + Serialize + for<'a> Deserialize<'a>,
    > FileHash<KeyType, ValueType>
{
    pub fn new() -> Self {
        Self {
            id2pos: HashMap::new(),
            file_handle: None,
            phantom: PhantomData,
            in_memory: HashMap::new(),
            disk_free: Vec::new(),
            max_mem_entries: MAX_MEM_ENTRIES,
            using_disk: false,
        }
    }

    pub fn len(&self) -> usize {
        self.in_memory.len() + self.id2pos.len()
    }

    pub fn is_empty(&self) -> bool {
        self.in_memory.is_empty() && self.id2pos.is_empty()
    }

    pub fn clear(&mut self) -> Result<()> {
        self.id2pos.clear();
        self.in_memory.clear();
        if let Some(fh_lock) = &self.file_handle {
            let fh = fh_lock
                .lock()
                .map_err(|e| anyhow!("Poisoned file handle in FileHash: {e}"))?;
            fh.set_len(0)?;
        }
        Ok(())
    }

    pub fn remove<K: Into<KeyType>>(&mut self, key: K) -> Option<ValueType> {
        if self.using_disk {
            let key = key.into();
            let value = self.get(key.clone());
            if value.is_some() {
                self.id2pos.remove(&key);
            }
            value
        } else {
            self.in_memory.remove(&key.into())
        }
    }

    pub fn swap<K1: Into<KeyType>, K2: Into<KeyType>>(&mut self, idx1: K1, idx2: K2) {
        let idx1: KeyType = idx1.into();
        let idx2: KeyType = idx2.into();
        if idx1 == idx2 {
            return;
        }
        if self.using_disk {
            self.swap_disk(idx1, idx2);
        } else {
            self.swap_mem(idx1, idx2);
        }
    }

    fn swap_mem(&mut self, idx1: KeyType, idx2: KeyType) {
        let v1 = self.in_memory.remove(&idx1);
        let v2 = self.in_memory.remove(&idx2);

        match (v1, v2) {
            (Some(value), None) => {
                self.in_memory.insert(idx2, value);
            }
            (None, Some(value)) => {
                self.in_memory.insert(idx1, value);
            }
            (Some(v1), Some(v2)) => {
                self.in_memory.insert(idx1, v2);
                self.in_memory.insert(idx2, v1);
            }
            (None, None) => {}
        }
    }

    fn swap_disk(&mut self, idx1: KeyType, idx2: KeyType) {
        let v1 = self.id2pos.remove(&idx1);
        let v2 = self.id2pos.remove(&idx2);

        match (v1, v2) {
            (Some(value), None) => {
                self.id2pos.insert(idx2, value);
            }
            (None, Some(value)) => {
                self.id2pos.insert(idx1, value);
            }
            (Some(v1), Some(v2)) => {
                self.id2pos.insert(idx1, v2);
                self.id2pos.insert(idx2, v1);
            }
            (None, None) => {}
        }
    }

    /// Adds an entity for the key.
    /// Note that this will overwrite any existing entity for the key.
    /// Also, this will add the new value at the end of the file, wasting space for the previous one.
    pub fn insert<K: Into<KeyType>, V: Into<ValueType>>(&mut self, key: K, value: V) -> Result<()> {
        let key: KeyType = key.into();
        let value: ValueType = value.into();
        if self.using_disk {
            self.insert_disk(key, value)
        } else {
            self.insert_mem(key, value)
        }
    }

    fn insert_mem(&mut self, key: KeyType, value: ValueType) -> Result<()> {
        if self.in_memory.len() >= self.max_mem_entries {
            self.flush_mem_to_disk()?;
            self.insert_disk(key, value)
        } else {
            self.in_memory.insert(key.into(), value.into());
            Ok(())
        }
    }

    // Only used when `using_disk` is true
    fn get_file_pos_to_write(&mut self, fh: &File, size: u64) -> Result<u64> {
        let mut position = fh.metadata()?.len();
        for (num, (start, len)) in self.disk_free.iter_mut().enumerate() {
            if *len >= size {
                position = *start;

                // Poor man's memory management
                if *len == size {
                    self.disk_free.remove(num);
                } else {
                    *len -= size;
                    *start += size;
                }
                break;
            }
        }
        Ok(position)
    }

    // Only used when `using_disk` is true
    fn release_storage(&mut self, key: &KeyType) {
        if let Some((start, len)) = self.id2pos.remove(key) {
            self.disk_free.push((start, len));
        }
    }

    fn insert_disk(&mut self, key: KeyType, value: ValueType) -> Result<()> {
        let fh = self.get_or_create_file_handle()?;
        let mut fh = fh.lock().map_err(|e| anyhow!(format!("{e}")))?;
        self.release_storage(&key);

        let json = serde_json::to_string(&value)?;
        let bytes = json.as_bytes();
        let size = bytes.len() as u64;
        let position_in_file = self.get_file_pos_to_write(&fh, size)?;
        fh.seek(SeekFrom::Start(position_in_file))?;
        fh.write_all(bytes)?;
        self.id2pos.insert(key, (position_in_file, size));
        Ok(())
    }

    /// Flushes all in-memory entities to disk.
    /// This is done automatically when the number of in-memory entities exceeds the limit.
    /// This operation is final; the entities will not be kept in memory again.
    fn flush_mem_to_disk(&mut self) -> Result<()> {
        let keys = self.in_memory.keys().cloned().collect::<Vec<KeyType>>();
        for key in &keys {
            let v = self
                .in_memory
                .get(key)
                .ok_or_else(|| anyhow!("Key not found"))?;
            self.insert_disk(key.to_owned(), v.to_owned())?;
        }
        self.in_memory.clear();
        self.using_disk = true;
        Ok(())
    }

    pub fn contains<K: Into<KeyType>>(&self, key: K) -> bool {
        if self.using_disk {
            self.id2pos.contains_key(&key.into())
        } else {
            self.in_memory.contains_key(&key.into())
        }
    }

    pub fn get<K: Into<KeyType>>(&self, key: K) -> Option<ValueType> {
        let key = key.into();
        if self.using_disk {
            self.get_disk(key)
        } else {
            self.in_memory.get(&key).cloned()
        }
    }

    fn get_disk(&self, key: KeyType) -> Option<ValueType> {
        let mut fh = self.file_handle.as_ref()?.lock().ok()?;
        let (start, length) = self.id2pos.get(&key)?;
        fh.seek(SeekFrom::Start(*start)).ok()?;
        let mut buffer: Vec<u8> = vec![0; *length as usize];
        fh.read_exact(&mut buffer).ok()?;
        let s: String = String::from_utf8(buffer).ok()?;
        serde_json::from_str(&s).ok()
    }

    pub fn keys(&self) -> Vec<KeyType> {
        if self.using_disk {
            self.id2pos.keys().cloned().collect()
        } else {
            self.in_memory.keys().cloned().collect()
        }
    }

    pub fn retain<F>(&mut self, mut f: F)
    where
        F: FnMut(&KeyType, &ValueType) -> bool,
    {
        for key in self.keys() {
            let value = match self.get(key.clone()) {
                Some(v) => v,
                None => continue, // TODO continue if key not found? This should never happen but...
            };
            if !f(&key, &value) {
                self.remove(key);
            }
        }
    }

    fn get_or_create_file_handle(&mut self) -> Result<Arc<Mutex<File>>> {
        if let Some(fh) = &self.file_handle {
            return Ok(fh.clone());
        }
        let fh = tempfile()?;
        self.file_handle = Some(Arc::new(Mutex::new(fh))); // Should auto-destruct
        if let Some(fh) = &self.file_handle {
            return Ok(fh.clone());
        }
        Err(anyhow!(
            "FileHash::get_or_create_file_handle: This is weird"
        ))
    }

    pub fn set_max_mem_entries(&mut self, max_mem_entries: usize) {
        self.max_mem_entries = max_mem_entries;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_hash() {
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

    #[test]
    fn test_file_hash_swap() {
        let mut efc: FileHash<String, String> = FileHash::new();
        efc.insert("Q123", "Foo").unwrap();
        efc.insert("Q456", "Bar").unwrap();
        efc.insert("Q789", "Baz").unwrap();
        efc.swap("Q123", "Q789");
        assert_eq!(efc.get("Q123").unwrap(), "Baz");
        assert_eq!(efc.get("Q456").unwrap(), "Bar");
        assert_eq!(efc.get("Q789").unwrap(), "Foo");
    }

    #[test]
    fn test_file_hash_swap_disk() {
        let mut efc: FileHash<String, String> = FileHash::new();
        efc.set_max_mem_entries(0);
        efc.insert("Q123", "Foo").unwrap();
        efc.insert("Q456", "Bar").unwrap();
        efc.insert("Q789", "Baz").unwrap();
        efc.swap("Q123", "Q789");
        assert_eq!(efc.get("Q123").unwrap(), "Baz");
        assert_eq!(efc.get("Q456").unwrap(), "Bar");
        assert_eq!(efc.get("Q789").unwrap(), "Foo");
    }

    #[test]
    fn test_file_hash_remove() {
        let mut efc: FileHash<String, String> = FileHash::new();
        efc.insert("Q123", "Foo").unwrap();
        efc.insert("Q456", "Bar").unwrap();
        efc.insert("Q789", "Baz").unwrap();
        assert_eq!(efc.remove("Q456").unwrap(), "Bar");
        assert_eq!(efc.get("Q123").unwrap(), "Foo");
        assert_eq!(efc.get("Q456"), None);
        assert_eq!(efc.get("Q789").unwrap(), "Baz");
    }

    #[test]
    fn test_file_hash_remove_disk() {
        let mut efc: FileHash<String, String> = FileHash::new();
        efc.set_max_mem_entries(0);
        efc.insert("Q123", "Foo").unwrap();
        efc.insert("Q456", "Bar").unwrap();
        efc.insert("Q789", "Baz").unwrap();
        assert_eq!(efc.remove("Q456").unwrap(), "Bar");
        assert_eq!(efc.get("Q123").unwrap(), "Foo");
        assert_eq!(efc.get("Q456"), None);
        assert_eq!(efc.get("Q789").unwrap(), "Baz");
    }

    #[test]
    fn test_file_hash_retain() {
        let mut efc: FileHash<String, String> = FileHash::new();
        efc.insert("Q123", "Foo").unwrap();
        efc.insert("Q456", "Bar").unwrap();
        efc.insert("Q789", "Baz").unwrap();
        efc.retain(|_, v| v == "Bar");
        assert_eq!(efc.get("Q123"), None);
        assert_eq!(efc.get("Q456").unwrap(), "Bar");
        assert_eq!(efc.get("Q789"), None);
    }

    #[test]
    fn test_flush_mem_to_disk() {
        let mut efc: FileHash<String, String> = FileHash::new();
        efc.set_max_mem_entries(2);
        efc.insert("Q123", "Foo").unwrap();
        efc.insert("Q456", "Bar").unwrap();
        assert_eq!(efc.get("Q123").unwrap(), "Foo");
        assert_eq!(efc.get("Q456").unwrap(), "Bar");
        assert!(!efc.using_disk);
        efc.insert("Q789", "Baz").unwrap();
        assert!(efc.using_disk);
        assert!(efc.in_memory.is_empty());
        assert_eq!(efc.get("Q123").unwrap(), "Foo");
        assert_eq!(efc.get("Q456").unwrap(), "Bar");
        assert_eq!(efc.get("Q789").unwrap(), "Baz");
    }

    #[test]
    fn test_file_hash_clear() {
        let mut efc: FileHash<String, String> = FileHash::new();
        efc.insert("Q123", "Foo").unwrap();
        efc.insert("Q456", "Bar").unwrap();
        efc.insert("Q789", "Baz").unwrap();
        efc.clear().unwrap();
        assert!(efc.is_empty());
    }

    #[test]
    fn test_file_hash_keys() {
        let mut efc: FileHash<String, String> = FileHash::new();
        efc.insert("Q123", "Foo").unwrap();
        efc.insert("Q456", "Bar").unwrap();
        efc.insert("Q789", "Baz").unwrap();
        let mut keys = efc.keys();
        keys.sort();
        assert_eq!(keys, vec!["Q123", "Q456", "Q789"]);
    }

    #[test]
    fn test_file_hash_contains() {
        let mut efc: FileHash<String, String> = FileHash::new();
        efc.insert("Q123", "Foo").unwrap();
        efc.insert("Q456", "Bar").unwrap();
        efc.insert("Q789", "Baz").unwrap();
        assert!(efc.contains("Q123"));
        assert!(efc.contains("Q456"));
        assert!(efc.contains("Q789"));
        assert!(!efc.contains("Q000"));
    }

    #[test]
    fn test_file_hash_len() {
        let mut efc: FileHash<String, String> = FileHash::new();
        assert_eq!(efc.len(), 0);
        efc.insert("Q123", "Foo").unwrap();
        assert_eq!(efc.len(), 1);
        efc.insert("Q456", "Bar").unwrap();
        assert_eq!(efc.len(), 2);
        efc.insert("Q789", "Baz").unwrap();
        assert_eq!(efc.len(), 3);
    }

    #[test]
    fn test_file_hash_is_empty() {
        let mut efc: FileHash<String, String> = FileHash::new();
        assert!(efc.is_empty());
        efc.insert("Q123", "Foo").unwrap();
        assert!(!efc.is_empty());
        efc.clear().unwrap();
        assert!(efc.is_empty());
    }

    #[test]
    fn test_file_hash_alter_disk() {
        let mut efc: FileHash<String, String> = FileHash::new();
        efc.set_max_mem_entries(0);
        efc.insert("Q123", "Foo").unwrap();
        efc.insert("Q456", "Bar").unwrap();
        efc.insert("Q789", "Baz").unwrap();

        assert_eq!(efc.get("Q123").unwrap(), "Foo");
        assert_eq!(efc.get("Q456").unwrap(), "Bar");
        assert_eq!(efc.get("Q789").unwrap(), "Baz");

        efc.insert("Q456", "Bob").unwrap();
        assert_eq!(efc.get("Q123").unwrap(), "Foo");
        assert_eq!(efc.get("Q456").unwrap(), "Bob");
        assert_eq!(efc.get("Q789").unwrap(), "Baz");

        efc.insert("Q456", "Sm").unwrap();
        assert_eq!(efc.get("Q123").unwrap(), "Foo");
        assert_eq!(efc.get("Q456").unwrap(), "Sm");
        assert_eq!(efc.get("Q789").unwrap(), "Baz");

        efc.insert("Q456", "Boom").unwrap();
        assert_eq!(efc.get("Q123").unwrap(), "Foo");
        assert_eq!(efc.get("Q456").unwrap(), "Boom");
        assert_eq!(efc.get("Q789").unwrap(), "Baz");
    }
}
