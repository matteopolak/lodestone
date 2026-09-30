//! Section membership grouped by column for bounded readiness resets.

use std::collections::{HashMap, hash_map::Entry};

use super::SectionKey;

#[derive(Debug, Default)]
pub(super) struct ColumnSectionSet {
    columns: HashMap<(i32, i32), ColumnMembership>,
}

#[derive(Debug)]
struct ColumnMembership {
    first: HeightMembership,
    others: Vec<HeightMembership>,
}

impl ColumnMembership {
    fn new(min_y: i32) -> Self {
        Self { first: HeightMembership::new(min_y), others: Vec::new() }
    }

    fn insert(&mut self, key: SectionKey) -> bool {
        if self.first.min_y == key.min_y {
            return self.first.insert(key.si);
        }
        if let Some(group) = self.others.iter_mut().find(|group| group.min_y == key.min_y) {
            return group.insert(key.si);
        }
        let mut group = HeightMembership::new(key.min_y);
        group.insert(key.si);
        self.others.push(group);
        true
    }

    fn contains(&self, key: &SectionKey) -> bool {
        if self.first.min_y == key.min_y {
            return self.first.contains(key.si);
        }
        self.others.iter().find(|group| group.min_y == key.min_y)
            .is_some_and(|group| group.contains(key.si))
    }

    fn remove(&mut self, key: &SectionKey) -> bool {
        if self.first.min_y == key.min_y {
            let removed = self.first.remove(key.si);
            if self.first.is_empty() {
                if let Some(group) = self.others.pop() {
                    self.first = group;
                }
                if self.others.is_empty() {
                    self.others = Vec::new();
                }
            }
            return removed;
        }
        let Some(index) = self.others.iter().position(|group| group.min_y == key.min_y) else {
            return false;
        };
        let removed = self.others[index].remove(key.si);
        if self.others[index].is_empty() {
            self.others.swap_remove(index);
            if self.others.is_empty() {
                self.others = Vec::new();
            }
        }
        removed
    }

    fn is_empty(&self) -> bool {
        self.first.is_empty() && self.others.is_empty()
    }
}

#[derive(Debug)]
struct HeightMembership {
    min_y: i32,
    low: u64,
    high: Vec<u64>,
}

impl HeightMembership {
    fn new(min_y: i32) -> Self {
        Self { min_y, low: 0, high: Vec::new() }
    }

    fn insert(&mut self, si: usize) -> bool {
        let word_index = si / u64::BITS as usize;
        let mask = 1u64 << (si % u64::BITS as usize);
        let word = if word_index == 0 {
            &mut self.low
        } else {
            if self.high.len() < word_index {
                self.high.resize(word_index, 0);
            }
            &mut self.high[word_index - 1]
        };
        let inserted = *word & mask == 0;
        *word |= mask;
        inserted
    }

    fn contains(&self, si: usize) -> bool {
        let word_index = si / u64::BITS as usize;
        let word = if word_index == 0 {
            self.low
        } else {
            self.high.get(word_index - 1).copied().unwrap_or(0)
        };
        word & (1u64 << (si % u64::BITS as usize)) != 0
    }

    fn remove(&mut self, si: usize) -> bool {
        let word_index = si / u64::BITS as usize;
        let mask = 1u64 << (si % u64::BITS as usize);
        let word = if word_index == 0 {
            &mut self.low
        } else {
            let Some(word) = self.high.get_mut(word_index - 1) else {
                return false;
            };
            word
        };
        let removed = *word & mask != 0;
        *word &= !mask;
        while self.high.last() == Some(&0) {
            self.high.pop();
        }
        removed
    }

    fn is_empty(&self) -> bool {
        self.low == 0 && self.high.is_empty()
    }
}

impl ColumnSectionSet {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn insert(&mut self, key: SectionKey) -> bool {
        self.columns.entry((key.cx, key.cz))
            .or_insert_with(|| ColumnMembership::new(key.min_y)).insert(key)
    }

    pub(super) fn contains(&self, key: &SectionKey) -> bool {
        self.columns.get(&(key.cx, key.cz)).is_some_and(|column| column.contains(key))
    }

    pub(super) fn remove(&mut self, key: &SectionKey) -> bool {
        let Entry::Occupied(mut column) = self.columns.entry((key.cx, key.cz)) else {
            return false;
        };
        let removed = column.get_mut().remove(key);
        if column.get().is_empty() {
            column.remove();
        }
        removed
    }

    pub(super) fn remove_column(&mut self, cx: i32, cz: i32) {
        self.columns.remove(&(cx, cz));
    }

    pub(super) fn clear(&mut self) {
        self.columns.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn membership_preserves_section_identity_and_prunes_empty_columns() {
        let key = SectionKey { cx: -2, cz: 3, si: 4, min_y: -64 };
        let other_height = SectionKey { min_y: 0, ..key };
        let mut sections = ColumnSectionSet::new();

        assert!(sections.insert(key));
        assert!(!sections.insert(key));
        assert!(!sections.contains(&other_height));
        assert!(sections.insert(other_height));
        assert!(sections.remove(&key));
        assert!(!sections.remove(&key));
        assert!(sections.contains(&other_height));
        assert_eq!(sections.columns.len(), 1);
        assert!(sections.remove(&other_height));
        assert!(sections.columns.is_empty());
    }

    #[test]
    fn column_removal_preserves_unrelated_membership_and_clear_drops_all() {
        let key = SectionKey { cx: -2, cz: 3, si: 4, min_y: -64 };
        let other_height = SectionKey { min_y: 0, ..key };
        let other_x = SectionKey { cx: 2, ..key };
        let other_z = SectionKey { cz: -3, ..key };
        let mut sections = ColumnSectionSet::new();
        for member in [key, other_height, other_x, other_z] {
            assert!(sections.insert(member));
        }

        sections.remove_column(key.cx, key.cz);
        sections.remove_column(key.cx, key.cz);
        assert!(!sections.contains(&key));
        assert!(!sections.contains(&other_height));
        assert!(sections.contains(&other_x));
        assert!(sections.contains(&other_z));
        assert_eq!(sections.columns.len(), 2);
        sections.clear();
        assert!(sections.columns.is_empty());
    }

    #[test]
    fn ordinary_column_membership_has_no_inner_allocation() {
        let key = SectionKey { cx: 0, cz: 0, si: 0, min_y: -64 };
        let mut sections = ColumnSectionSet::new();
        for si in 0..24 {
            assert!(sections.insert(SectionKey { si, ..key }));
        }
        for si in 0..64 {
            assert_eq!(sections.contains(&SectionKey { si, ..key }), si < 24);
        }
        let column = &sections.columns[&(key.cx, key.cz)];
        assert_eq!(column.first.low, (1u64 << 24) - 1);
        assert_eq!(column.first.high.capacity(), 0);
        assert_eq!(column.others.capacity(), 0);
        let inline_bytes = std::mem::size_of::<ColumnMembership>();
        let section_key_bytes = 24 * std::mem::size_of::<SectionKey>();
        assert!(inline_bytes < section_key_bytes);
        eprintln!(
            "24-section membership: inline_bytes={inline_bytes} inner_heap_bytes=0 \
             separate_section_key_bytes={section_key_bytes}",
        );
    }

    #[test]
    fn tall_column_words_preserve_membership_at_boundaries() {
        let key = SectionKey { cx: 0, cz: 0, si: 0, min_y: -64 };
        let indices = [0, 63, 64, 127, 130];
        let mut sections = ColumnSectionSet::new();
        for si in indices {
            assert!(sections.insert(SectionKey { si, ..key }));
            assert!(!sections.insert(SectionKey { si, ..key }));
        }
        for si in 0..192 {
            assert_eq!(sections.contains(&SectionKey { si, ..key }), indices.contains(&si));
        }
        let column = &sections.columns[&(key.cx, key.cz)];
        assert_eq!(column.first.low, 1 | (1u64 << 63));
        assert_eq!(column.first.high, vec![1 | (1u64 << 63), 1u64 << 2]);
        for si in indices.into_iter().rev() {
            assert!(sections.remove(&SectionKey { si, ..key }));
            assert!(!sections.remove(&SectionKey { si, ..key }));
        }
        assert!(sections.columns.is_empty());
    }

    #[test]
    fn removing_an_exceptional_height_group_keeps_other_groups() {
        let key = SectionKey { cx: 0, cz: 0, si: 4, min_y: -64 };
        let middle = SectionKey { min_y: 0, ..key };
        let last = SectionKey { min_y: 64, ..key };
        let mut sections = ColumnSectionSet::new();
        for member in [key, middle, last] {
            assert!(sections.insert(member));
        }
        assert!(sections.remove(&middle));
        assert!(!sections.contains(&middle));
        assert!(sections.contains(&key));
        assert!(sections.contains(&last));
        assert_eq!(sections.columns[&(key.cx, key.cz)].others.len(), 1);
        assert!(sections.remove(&key));
        assert!(sections.contains(&last));
        assert_eq!(sections.columns[&(key.cx, key.cz)].others.capacity(), 0);
        assert!(sections.remove(&last));
        assert!(sections.columns.is_empty());
    }
}
