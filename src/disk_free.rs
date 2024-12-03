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
        let mut last_position = 0;
        let mut last_index = 0;
        let new_end = new_pl.position + new_pl.length;
        for (idx, pl) in self.parts.iter_mut().enumerate() {
            if new_end == pl.position {
                pl.position = new_pl.position;
                pl.length += new_pl.length;
                return;
            }
            if pl.end() == new_pl.position {
                pl.length += new_pl.length;
                return;
            }
            if pl.position >= last_position && pl.position < new_pl.position {
                last_position = pl.position;
                last_index = idx;
            }
        }
        self.parts.insert(last_index + 1, new_pl);
    }

    pub fn find_free(&mut self, size: u64) -> Option<u64> {
        let equal_idx = self
            .parts
            .iter()
            .enumerate()
            .filter(|(_num, part)| part.length == size)
            .map(|(num, _part)| num)
            .next();
        if let Some(num) = equal_idx {
            let position = Some(self.parts[num].position);
            self.parts.remove(num);
            return position;
        }

        for part in self.parts.iter_mut() {
            if part.length >= size {
                let position = Some(part.position);
                part.length -= size;
                part.position += size;
                return position;
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
