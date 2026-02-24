use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

use crate::file_hash::FileHash;

#[derive(Debug, Clone)]
pub struct FileVec<ValueType> {
    file_hash: FileHash<usize, ValueType>,
    len: usize,
}

impl<ValueType: Clone + Serialize + for<'a> Deserialize<'a>> FileVec<ValueType> {
    pub fn new() -> Self {
        Self {
            file_hash: FileHash::new(),
            len: 0,
        }
    }

    pub fn push(&mut self, row: ValueType) {
        self.file_hash
            .insert(self.len, row)
            .expect("Failed to push result");
        self.len += 1;
    }

    pub fn get(&self, pos: usize) -> Option<ValueType> {
        self.file_hash.get(pos)
    }

    pub fn set(&mut self, pos: usize, row: ValueType) -> Result<()> {
        if pos < self.len {
            self.file_hash
                .insert(pos, row)
                .expect("Failed to set result");
            Ok(())
        } else {
            Err(anyhow!("Attempting to set out-of-bounds result {pos}"))
        }
    }

    pub fn clear(&mut self) -> Result<()> {
        self.file_hash.clear()?;
        self.len = 0;
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn remove(&mut self, idx: usize) -> Option<ValueType> {
        if idx >= self.len {
            return None;
        }
        // Shift all elements after idx to the left
        for pos in (idx + 1)..self.len {
            self.swap(pos - 1, pos).ok()?;
        }
        self.pop()
    }

    pub fn pop(&mut self) -> Option<ValueType> {
        if self.len > 0 {
            self.len -= 1;
            self.file_hash.remove(self.len)
        } else {
            None
        }
    }

    pub fn retain<F>(&mut self, mut f: F)
    where
        F: FnMut(&ValueType) -> bool,
    {
        let mut write_pos = 0;
        for read_pos in 0..self.len {
            let value = self
                .get(read_pos)
                .expect("FileVec::keep_marked: row not found");
            if f(&value) {
                self.file_hash.swap(read_pos, write_pos);
                write_pos += 1;
            }
        }
        while self.len > write_pos {
            self.pop();
        }
    }

    pub fn sort_by<F>(&mut self, mut f: F) -> Result<()>
    where
        F: FnMut(&ValueType, &ValueType) -> Ordering,
    {
        let n = self.len;
        for i in 0..n {
            let mut min_idx = i;
            for j in i + 1..n {
                let row_j = self.get(j).expect("FileVec::sort_by: row not found");
                let row_min_idx = self.get(min_idx).expect("FileVec::sort_by: row not found");
                if f(&row_j, &row_min_idx) == Ordering::Less {
                    min_idx = j;
                }
            }
            self.swap(i, min_idx)?;
        }
        Ok(())
    }

    fn swap(&mut self, idx1: usize, idx2: usize) -> Result<()> {
        if idx1 >= self.len || idx2 >= self.len {
            return Err(anyhow!("FileVec::swap: Attempting to swap out-of-bounds"));
        }
        // Delegate to FileHash::swap, which only exchanges two PositionLength entries
        // in the in-memory HashMap — no disk I/O required.  The equal-index case is
        // handled inside FileHash::swap itself.
        self.file_hash.swap(idx1, idx2);
        Ok(())
    }

    pub fn reverse(&mut self) -> Result<()> {
        let mut front = 0;
        let mut end = self.len - 1;
        while front < end {
            self.swap(front, end)?;
            front += 1;
            end -= 1;
        }
        Ok(())
    }
}

impl<ValueType: Clone + Serialize + for<'a> Deserialize<'a>> Default for FileVec<ValueType> {
    fn default() -> Self {
        Self::new()
    }
}

/// CAUTION:
/// This implementation of `IntoIterator` DOES NOT consume the `FileVec` object.
/// I couldn't get iter() to work so just use `into_iter()` instead.
/// It returns values rather than references but they are just copies.
impl<'a, ValueType> IntoIterator for &'a FileVec<ValueType>
where
    ValueType: Clone + Serialize + for<'de> Deserialize<'de>,
{
    type Item = ValueType;
    type IntoIter = FileVecIterator<'a, ValueType>;

    fn into_iter(self) -> Self::IntoIter {
        FileVecIterator {
            file_vec: self,
            index: 0,
        }
    }
}

pub struct FileVecIterator<'a, ValueType> {
    file_vec: &'a FileVec<ValueType>,
    index: usize,
}

impl<'a, ValueType> Iterator for FileVecIterator<'a, ValueType>
where
    ValueType: Clone + Serialize + for<'de> Deserialize<'de>,
{
    type Item = ValueType;
    fn next(&mut self) -> Option<ValueType> {
        if self.index < self.file_vec.len {
            self.index += 1;
            self.file_vec.get(self.index - 1)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reverse() {
        let mut file_vec: FileVec<String> = FileVec::new();
        file_vec.push("a".to_string());
        file_vec.push("b".to_string());
        file_vec.push("c".to_string());
        file_vec.reverse().unwrap();
        assert_eq!(file_vec.len(), 3);
        assert_eq!(file_vec.get(0).unwrap(), "c");
        assert_eq!(file_vec.get(1).unwrap(), "b");
        assert_eq!(file_vec.get(2).unwrap(), "a");
    }

    #[test]
    fn test_sort_by_empty() {
        let mut file_vec: FileVec<String> = FileVec::new();
        file_vec.sort_by(|a, b| a.cmp(b)).unwrap();
    }

    #[test]
    fn test_sort_by_single() {
        let mut file_vec: FileVec<String> = FileVec::new();
        file_vec.push("a".to_string());
        file_vec.sort_by(|a, b| a.cmp(b)).unwrap();
    }

    #[test]
    fn test_sort_by_two() {
        let mut file_vec: FileVec<String> = FileVec::new();
        file_vec.push("a".to_string());
        file_vec.push("b".to_string());
        file_vec.sort_by(|a, b| a.cmp(b)).unwrap();
    }

    #[test]
    fn test_sort_by() {
        let mut file_vec: FileVec<String> = FileVec::new();
        file_vec.push("c".to_string());
        file_vec.push("b".to_string());
        file_vec.push("a".to_string());
        file_vec.sort_by(|a, b| a.cmp(b)).unwrap();
        assert_eq!(file_vec.len(), 3);
        assert_eq!(file_vec.get(0).unwrap(), "a");
        assert_eq!(file_vec.get(1).unwrap(), "b");
        assert_eq!(file_vec.get(2).unwrap(), "c");
    }

    #[test]
    fn test_clear() {
        let mut file_vec: FileVec<String> = FileVec::new();
        file_vec.push("a".to_string());
        file_vec.push("b".to_string());
        file_vec.push("c".to_string());
        file_vec.clear().unwrap();
        assert_eq!(file_vec.len(), 0);
    }

    #[test]
    fn test_file_vec_push() {
        let mut file_vec: FileVec<String> = FileVec::new();
        file_vec.push("a".to_string());
        file_vec.push("b".to_string());
        file_vec.push("c".to_string());
        assert_eq!(file_vec.len(), 3);
        assert_eq!(file_vec.get(0).unwrap(), "a");
        assert_eq!(file_vec.get(1).unwrap(), "b");
        assert_eq!(file_vec.get(2).unwrap(), "c");
    }

    #[test]
    fn test_set() {
        let mut file_vec: FileVec<String> = FileVec::new();
        file_vec.push("a".to_string());
        file_vec.push("b".to_string());
        file_vec.push("c".to_string());
        file_vec.set(1, "d".to_string()).unwrap();
        assert_eq!(file_vec.len(), 3);
        assert_eq!(file_vec.get(0).unwrap(), "a");
        assert_eq!(file_vec.get(1).unwrap(), "d");
        assert_eq!(file_vec.get(2).unwrap(), "c");
    }

    #[test]
    fn test_get() {
        let mut file_vec: FileVec<String> = FileVec::new();
        file_vec.push("a".to_string());
        file_vec.push("b".to_string());
        file_vec.push("c".to_string());
        assert_eq!(file_vec.get(0).unwrap(), "a");
        assert_eq!(file_vec.get(1).unwrap(), "b");
        assert_eq!(file_vec.get(2).unwrap(), "c");
    }

    #[test]
    fn test_is_empty() {
        let mut file_vec: FileVec<String> = FileVec::new();
        assert!(file_vec.is_empty());
        file_vec.push("a".to_string());
        assert!(!file_vec.is_empty());
    }

    #[test]
    fn test_len() {
        let mut file_vec: FileVec<String> = FileVec::new();
        assert_eq!(file_vec.len(), 0);
        file_vec.push("a".to_string());
        assert_eq!(file_vec.len(), 1);
    }

    #[test]
    fn test_remove() {
        let mut file_vec: FileVec<String> = FileVec::new();
        file_vec.push("a".to_string());
        file_vec.push("b".to_string());
        file_vec.push("c".to_string());
        file_vec.remove(1);
        assert_eq!(file_vec.len(), 2);
        assert_eq!(file_vec.get(0).unwrap(), "a");
        assert_eq!(file_vec.get(1).unwrap(), "c");
    }

    #[test]
    fn test_remove_last() {
        let mut file_vec: FileVec<String> = FileVec::new();
        file_vec.push("a".to_string());
        file_vec.push("b".to_string());
        file_vec.push("c".to_string());
        file_vec.remove(2);
        assert_eq!(file_vec.len(), 2);
        assert_eq!(file_vec.get(0).unwrap(), "a");
        assert_eq!(file_vec.get(1).unwrap(), "b");
    }

    #[test]
    fn test_remove_out_of_bounds() {
        let mut file_vec: FileVec<String> = FileVec::new();
        file_vec.push("a".to_string());
        file_vec.push("b".to_string());
        file_vec.push("c".to_string());
        assert_eq!(file_vec.remove(3), None);
    }

    #[test]
    fn test_swap() {
        let mut file_vec: FileVec<String> = FileVec::new();
        file_vec.push("a".to_string());
        file_vec.push("b".to_string());
        file_vec.push("c".to_string());
        file_vec.swap(0, 2).unwrap();
        assert_eq!(file_vec.len(), 3);
        assert_eq!(file_vec.get(0).unwrap(), "c");
        assert_eq!(file_vec.get(1).unwrap(), "b");
        assert_eq!(file_vec.get(2).unwrap(), "a");
    }

    #[test]
    fn test_swap_out_of_bounds() {
        let mut file_vec: FileVec<String> = FileVec::new();
        assert!(file_vec.swap(0, 2).is_err());
    }

    #[test]
    fn test_swap_same_index() {
        let mut file_vec: FileVec<String> = FileVec::new();
        file_vec.push("a".to_string());
        file_vec.push("b".to_string());
        file_vec.push("c".to_string());
        file_vec.swap(0, 0).unwrap();
        assert_eq!(file_vec.len(), 3);
        assert_eq!(file_vec.get(0).unwrap(), "a");
        assert_eq!(file_vec.get(1).unwrap(), "b");
        assert_eq!(file_vec.get(2).unwrap(), "c");
    }

    #[test]
    fn test_swap_same_index_out_of_bounds() {
        let mut file_vec: FileVec<String> = FileVec::new();
        assert!(file_vec.swap(0, 5).is_err());
    }

    #[test]
    fn test_pop() {
        let mut file_vec: FileVec<String> = FileVec::new();
        file_vec.push("a".to_string());
        file_vec.push("b".to_string());
        file_vec.push("c".to_string());
        assert_eq!(file_vec.pop().unwrap(), "c");
        assert_eq!(file_vec.len(), 2);
        assert_eq!(file_vec.pop().unwrap(), "b");
        assert_eq!(file_vec.len(), 1);
        assert_eq!(file_vec.pop().unwrap(), "a");
        assert_eq!(file_vec.len(), 0);
    }

    #[test]
    fn test_pop_empty() {
        let mut file_vec: FileVec<String> = FileVec::new();
        assert_eq!(file_vec.pop(), None);
    }

    #[test]
    fn test_retain() {
        let mut file_vec: FileVec<String> = FileVec::new();
        file_vec.push("a".to_string());
        file_vec.push("b".to_string());
        file_vec.push("c".to_string());
        file_vec.retain(|x| x != "b");
        assert_eq!(file_vec.len(), 2);
        assert_eq!(file_vec.get(0).unwrap(), "a");
        assert_eq!(file_vec.get(1).unwrap(), "c");
    }

    #[test]
    fn test_into_iter() {
        let mut file_vec: FileVec<String> = FileVec::new();
        file_vec.push("a".to_string());
        file_vec.push("b".to_string());
        file_vec.push("c".to_string());
        let mut iter = file_vec.into_iter();
        assert_eq!(iter.next().unwrap(), "a");
        assert_eq!(iter.next().unwrap(), "b");
        assert_eq!(iter.next().unwrap(), "c");
        assert_eq!(iter.next(), None);

        // Test that it can be used again
        let mut iter = file_vec.into_iter();
        assert_eq!(iter.next().unwrap(), "a");
        assert_eq!(iter.next().unwrap(), "b");
        assert_eq!(iter.next().unwrap(), "c");
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn test_into_iter_empty() {
        let file_vec: FileVec<String> = FileVec::new();
        let mut iter = file_vec.into_iter();
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn test_into_iter_single() {
        let mut file_vec: FileVec<String> = FileVec::new();
        file_vec.push("a".to_string());
        let mut iter = file_vec.into_iter();
        assert_eq!(iter.next().unwrap(), "a");
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn test_into_iter_borrowed() {
        let mut file_vec: FileVec<String> = FileVec::new();
        file_vec.push("a".to_string());
        file_vec.push("b".to_string());
        file_vec.push("c".to_string());
        let mut iter = (&file_vec).into_iter();
        assert_eq!(iter.next().unwrap(), "a");
        assert_eq!(iter.next().unwrap(), "b");
        assert_eq!(iter.next().unwrap(), "c");
        assert_eq!(iter.next(), None);

        // Test that it can be used again
        let mut iter = (&file_vec).into_iter();
        assert_eq!(iter.next().unwrap(), "a");
        assert_eq!(iter.next().unwrap(), "b");
        assert_eq!(iter.next().unwrap(), "c");
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn test_into_iter_borrowed_empty() {
        let file_vec: FileVec<String> = FileVec::new();
        let mut iter = (&file_vec).into_iter();
        assert_eq!(iter.next(), None);
    }
}
