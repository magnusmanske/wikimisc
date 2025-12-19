/// `DiskFree` manages disk-based storage layout

#[derive(Clone, Debug)]
pub struct PositionLength {
    position: u64,
    length: u64,
}

impl PositionLength {
    pub fn new(position: u64, length: u64) -> Self {
        Self { position, length }
    }

    pub fn position(&self) -> u64 {
        self.position
    }

    pub fn length(&self) -> u64 {
        self.length
    }

    fn end(&self) -> u64 {
        self.position + self.length
    }
}

#[derive(Clone, Debug)]
pub struct DiskFree {
    parts: Vec<PositionLength>,
}

impl DiskFree {
    pub fn new() -> Self {
        Self { parts: Vec::new() }
    }

    pub fn add(&mut self, new_pl: PositionLength) {
        if self.parts.is_empty() {
            self.parts.push(new_pl);
            return;
        }

        let new_end = new_pl.position + new_pl.length;

        // Check if we can merge with an existing part
        for pl in &mut self.parts {
            if new_end == pl.position {
                // New part comes right before this part
                pl.position = new_pl.position;
                pl.length += new_pl.length;
                return;
            }
            if pl.end() == new_pl.position {
                // New part comes right after this part
                pl.length += new_pl.length;
                return;
            }
        }

        // Find insertion position
        let insert_idx = self
            .parts
            .iter()
            .enumerate()
            .filter(|(_, pl)| pl.position < new_pl.position)
            .map(|(idx, _)| idx)
            .next_back()
            .map_or(0, |idx| idx + 1);

        self.parts.insert(insert_idx, new_pl);
    }

    pub fn find_free(&mut self, size: u64) -> Option<u64> {
        // First, try to find an exact match
        if let Some(idx) = self.parts.iter().position(|part| part.length == size) {
            let position = self.parts[idx].position;
            self.parts.remove(idx);
            return Some(position);
        }

        // Otherwise, find first part large enough
        for part in &mut self.parts {
            if part.length >= size {
                let position = part.position;
                part.length -= size;
                part.position += size;
                return Some(position);
            }
        }
        None
    }
}

impl Default for DiskFree {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_disk_free_add() {
        let mut df = DiskFree::new();
        df.add(PositionLength::new(50, 10));
        df.add(PositionLength::new(10, 10));
        df.add(PositionLength::new(30, 10));
        assert_eq!(df.parts.len(), 3);
    }

    #[test]
    fn test_disk_free_find_free() {
        let mut df = DiskFree::new();
        df.add(PositionLength::new(50, 40));
        df.add(PositionLength::new(10, 10));
        df.add(PositionLength::new(30, 15));
        assert_eq!(df.find_free(41), None);
        assert_eq!(df.find_free(40), Some(50));
        assert_eq!(df.find_free(16), None);
        assert_eq!(df.find_free(15), Some(30));
        assert_eq!(df.find_free(11), None);
        assert_eq!(df.find_free(10), Some(10));
        assert_eq!(df.find_free(1), None);
    }

    #[test]
    fn test_disk_free_join_adjacent() {
        let mut df = DiskFree::new();
        df.add(PositionLength::new(20, 40));
        df.add(PositionLength::new(10, 10));
        assert_eq!(df.parts.len(), 1);
        assert_eq!(df.parts[0].position, 10);
        assert_eq!(df.parts[0].length, 50);
    }

    #[test]
    fn test_disk_free_join_adjacent2() {
        let mut df = DiskFree::new();
        df.add(PositionLength::new(10, 10));
        df.add(PositionLength::new(20, 40));
        assert_eq!(df.parts.len(), 1);
        assert_eq!(df.parts[0].position, 10);
        assert_eq!(df.parts[0].length, 50);
    }
}
