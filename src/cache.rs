//! Small async caches for values fetched now and then from elsewhere.
//!
//! Concurrent callers wait for one fetch instead of each making their own.
//! Keys and values are kept at most `max_age`; the map variants also bound
//! how many entries they hold.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, LazyLock, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// One cached value.
pub struct TimedCache<T> {
    max_age: Duration,
    slot: Mutex<Option<(Instant, Arc<T>)>>,
}

impl<T> TimedCache<T> {
    pub const fn new(max_age: Duration) -> Self {
        Self {
            max_age,
            slot: Mutex::const_new(None),
        }
    }

    /// The cached value, if it is fresh and no fetch holds the cache right now.
    pub fn peek(&self) -> Option<Arc<T>> {
        let slot = self.slot.try_lock().ok()?;
        slot.as_ref()
            .filter(|(fetched, _)| fetched.elapsed() < self.max_age)
            .map(|(_, value)| value.clone())
    }

    /// The cached value, or a fresh one from `fetch`. Concurrent callers wait
    /// for one fetch rather than each making their own.
    pub async fn get_or_fetch<E>(
        &self,
        fetch: impl AsyncFnOnce() -> Result<T, E>,
    ) -> Result<Arc<T>, E> {
        let mut slot = self.slot.lock().await;
        if let Some((fetched, value)) = slot.as_ref() {
            if fetched.elapsed() < self.max_age {
                return Ok(value.clone());
            }
        }
        let value = Arc::new(fetch().await?);
        *slot = Some((Instant::now(), value.clone()));
        Ok(value)
    }
}

/// One value per key, such as per wiki. Keys come from requests, so there
/// are at most `capacity`; when full, the cache starts over. A slow fetch
/// for one key does not hold up the others.
/// The values of a [`TimedMap`] and when each was fetched.
type Entries<K, V> = Mutex<Option<HashMap<K, (Instant, Arc<V>)>>>;

pub struct TimedMap<K, V> {
    max_age: Duration,
    capacity: usize,
    entries: Entries<K, V>,
}

impl<K: Eq + Hash, V> TimedMap<K, V> {
    pub const fn new(max_age: Duration, capacity: usize) -> Self {
        Self {
            max_age,
            capacity,
            entries: Mutex::const_new(None),
        }
    }

    /// The cached value for `key`, or a fresh one from `fetch`.
    pub async fn get_or_fetch<E>(
        &self,
        key: K,
        fetch: impl AsyncFnOnce() -> Result<V, E>,
    ) -> Result<Arc<V>, E> {
        if let Some((fetched, value)) = self.entries.lock().await.as_ref().and_then(|e| e.get(&key))
        {
            if fetched.elapsed() < self.max_age {
                return Ok(value.clone());
            }
        }
        let value = Arc::new(fetch().await?);
        let mut entries = self.entries.lock().await;
        let entries = entries.get_or_insert_with(HashMap::new);
        if entries.len() >= self.capacity {
            entries.clear();
        }
        entries.insert(key, (Instant::now(), value.clone()));
        Ok(value)
    }
}

/// Values by string key, with when they were fetched.
type Slots<T> = HashMap<String, (Instant, Arc<T>)>;

/// As [`TimedCache`], one value per key (e.g. per wiki); at most `max_keys`.
/// Concurrent callers may each fetch; the last one is kept.
pub struct KeyedCache<T> {
    max_age: Duration,
    max_keys: usize,
    slots: LazyLock<StdMutex<Slots<T>>>,
}

impl<T> KeyedCache<T> {
    pub const fn new(max_age: Duration, max_keys: usize) -> Self {
        Self {
            max_age,
            max_keys,
            slots: LazyLock::new(Default::default),
        }
    }

    /// The cached value for `key`, or a fresh one from `fetch`.
    pub async fn get_or_fetch<E>(
        &self,
        key: &str,
        fetch: impl AsyncFnOnce() -> Result<T, E>,
    ) -> Result<Arc<T>, E> {
        if let Some((fetched, value)) = self
            .slots
            .lock()
            .ok()
            .and_then(|slots| slots.get(key).cloned())
        {
            if fetched.elapsed() < self.max_age {
                return Ok(value);
            }
        }
        let value = Arc::new(fetch().await?);
        if let Ok(mut slots) = self.slots.lock() {
            slots.retain(|_, (fetched, _)| fetched.elapsed() < self.max_age);
            if slots.len() < self.max_keys {
                slots.insert(key.to_string(), (Instant::now(), value.clone()));
            }
        }
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fetches_once_until_stale() {
        let cache = TimedCache::new(Duration::from_secs(60));
        let first = cache.get_or_fetch(async || Ok::<_, ()>(1)).await.unwrap();
        let second = cache.get_or_fetch(async || Ok::<_, ()>(2)).await.unwrap();
        assert_eq!((*first, *second), (1, 1));
        assert_eq!(*cache.peek().unwrap(), 1);

        let stale = TimedCache::new(Duration::ZERO);
        stale.get_or_fetch(async || Ok::<_, ()>(1)).await.unwrap();
        assert_eq!(
            *stale.get_or_fetch(async || Ok::<_, ()>(2)).await.unwrap(),
            2
        );
        assert!(stale
            .get_or_fetch(async || Err::<i32, _>("down"))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn per_key_and_bounded() {
        let map = TimedMap::new(Duration::from_secs(60), 2);
        assert_eq!(
            *map.get_or_fetch("a", async || Ok::<_, ()>(1))
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            *map.get_or_fetch("b", async || Ok::<_, ()>(2))
                .await
                .unwrap(),
            2
        );
        assert_eq!(
            *map.get_or_fetch("a", async || Ok::<_, ()>(9))
                .await
                .unwrap(),
            1
        );
        // Full: starts over.
        assert_eq!(
            *map.get_or_fetch("c", async || Ok::<_, ()>(3))
                .await
                .unwrap(),
            3
        );
        assert_eq!(
            *map.get_or_fetch("a", async || Ok::<_, ()>(4))
                .await
                .unwrap(),
            4
        );
        assert!(map
            .get_or_fetch("d", async || Err::<i32, _>("down"))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn keyed_cache_keeps_one_value_per_key() {
        let cache = KeyedCache::new(Duration::from_secs(60), 2);
        assert_eq!(
            *cache
                .get_or_fetch("a", async || Ok::<_, ()>(1))
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            *cache
                .get_or_fetch("b", async || Ok::<_, ()>(2))
                .await
                .unwrap(),
            2
        );
        assert_eq!(
            *cache
                .get_or_fetch("a", async || Ok::<_, ()>(3))
                .await
                .unwrap(),
            1
        );
        // Full: fetched, but not kept.
        assert_eq!(
            *cache
                .get_or_fetch("c", async || Ok::<_, ()>(4))
                .await
                .unwrap(),
            4
        );
        assert_eq!(
            *cache
                .get_or_fetch("c", async || Ok::<_, ()>(5))
                .await
                .unwrap(),
            5
        );
    }
}
