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
        let new_end = new_pl.position + new_pl.length;

        // Find an existing part whose end touches the start of new_pl (new comes right after it).
        let before_idx = self.parts.iter().position(|pl| pl.end() == new_pl.position);

        // Find an existing part whose start touches the end of new_pl (new comes right before it).
        let after_idx = self.parts.iter().position(|pl| pl.position == new_end);

        match (before_idx, after_idx) {
            (Some(b), Some(a)) => {
                // New segment bridges two existing parts — merge all three into the "before" part.
                // Compute the end of the "after" part before we remove it.
                let after_end = self.parts[a].end();
                self.parts[b].length = after_end - self.parts[b].position;
                self.parts.remove(a);
                // If `a` came before `b` in the Vec, removing it shifts `b` down by one.
                // Nothing extra to do when b < a because we already mutated parts[b] first.
                // (The case b == a is impossible: the same part cannot both end at new_pl.position
                //  and start at new_end unless new_pl.length == 0, which is never valid.)
            }
            (Some(b), None) => {
                // New segment extends an existing part to the right.
                self.parts[b].length += new_pl.length;
            }
            (None, Some(a)) => {
                // New segment extends an existing part to the left.
                self.parts[a].position = new_pl.position;
                self.parts[a].length += new_pl.length;
            }
            (None, None) => {
                // No adjacent part — insert in sorted order by position.
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
        }
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

    #[test]
    fn test_disk_free_join_three_way() {
        // Three separate segments that should collapse into one when the middle is added.
        let mut df = DiskFree::new();
        df.add(PositionLength::new(0, 10)); // [0, 10)
        df.add(PositionLength::new(20, 10)); // [20, 30)
                                             // Adding the middle segment [10, 10) should bridge both and yield one part [0, 30).
        df.add(PositionLength::new(10, 10));
        assert_eq!(df.parts.len(), 1);
        assert_eq!(df.parts[0].position, 0);
        assert_eq!(df.parts[0].length, 30);
    }

    #[test]
    fn test_disk_free_join_three_way_reversed() {
        // Same as above but "before" part has a higher index than "after" part.
        let mut df = DiskFree::new();
        df.add(PositionLength::new(20, 10)); // [20, 30)
        df.add(PositionLength::new(0, 10)); // [0, 10)
        df.add(PositionLength::new(10, 10)); // [10, 20) — bridges both
        assert_eq!(df.parts.len(), 1);
        assert_eq!(df.parts[0].position, 0);
        assert_eq!(df.parts[0].length, 30);
    }
}
