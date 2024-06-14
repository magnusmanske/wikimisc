/// `DiskFree` manages disk-based storage layout

#[derive(Clone, Debug, PartialEq)]
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
}

impl Eq for PositionLength {}

impl PartialOrd for PositionLength {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.position.cmp(&other.position))
    }
}

impl Ord for PositionLength {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.position.cmp(&other.position)
    }
}

#[derive(Clone, Debug)]
pub struct DiskFree {
    parts: Vec<PositionLength>, // NOTE: This will always be sorted by position
}

impl DiskFree {
    pub fn new() -> Self {
        Self { parts: Vec::new() }
    }

    pub fn add(&mut self, pl: PositionLength) {
        self.parts.push(pl);
        self.parts.sort();
        self.join_adjacent();
    }

    fn join_adjacent(&mut self) {
        let mut idx = 1;
        while idx < self.parts.len() {
            let prev = &self.parts[idx - 1];
            let curr = &self.parts[idx];
            if prev.position + prev.length == curr.position {
                self.parts[idx - 1].length += curr.length;
                self.parts.remove(idx);
            } else {
                idx += 1;
            }
        }
    }

    pub fn find_free(&mut self, size: u64) -> Option<u64> {
        let mut position = None;
        for (num, part) in self.parts.iter_mut().enumerate() {
            if part.length >= size {
                position = Some(part.position);

                // Poor man's memory management
                if part.length == size {
                    self.parts.remove(num);
                } else {
                    part.length -= size;
                    part.position += size;
                }
                break;
            }
        }
        position
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
        assert_eq!(df.parts[0].position, 10);
        assert_eq!(df.parts[1].position, 30);
        assert_eq!(df.parts[2].position, 50);
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
    fn test_join_adjacent() {
        let mut df = DiskFree::new();
        df.add(PositionLength::new(20, 40));
        df.add(PositionLength::new(10, 10));
        assert_eq!(df.parts.len(), 1);
        assert_eq!(df.parts[0].position, 10);
        assert_eq!(df.parts[0].length, 50);
    }
}
