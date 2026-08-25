//! Small typed arenas used by HIR and semantic side tables.

use std::fmt;
use std::marker::PhantomData;
use std::ops::{Index, IndexMut};

use serde::{Deserialize, Serialize};

/// A typed index that can address an [`Arena`] or [`ArenaMap`].
pub trait ArenaId: Copy + Eq + Ord + std::hash::Hash + fmt::Debug {
    /// Creates an ID from a dense zero-based index.
    fn from_raw(raw: u32) -> Self;

    /// Returns the dense zero-based index.
    fn into_raw(self) -> u32;

    /// Converts the ID into a platform index.
    fn index(self) -> usize {
        self.into_raw() as usize
    }
}

/// An append-only arena with a distinct ID type.
#[derive(Clone, Deserialize, Serialize)]
pub struct Arena<I, T> {
    values: Vec<T>,
    marker: PhantomData<fn(I) -> I>,
}

impl<I, T> Default for Arena<I, T> {
    fn default() -> Self {
        Self {
            values: Vec::new(),
            marker: PhantomData,
        }
    }
}

impl<I, T> fmt::Debug for Arena<I, T>
where
    I: ArenaId,
    T: fmt::Debug,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_map().entries(self.iter()).finish()
    }
}

impl<I: ArenaId, T> Arena<I, T> {
    /// Appends a value and returns its typed ID.
    ///
    /// # Panics
    ///
    /// Panics if more than `u32::MAX` values are allocated in one arena.
    pub fn alloc(&mut self, value: T) -> I {
        let raw =
            u32::try_from(self.values.len()).expect("arena contains more than u32::MAX items");
        self.values.push(value);
        I::from_raw(raw)
    }

    /// Returns the number of allocated values.
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Returns whether no values have been allocated.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Gets a value by typed ID.
    #[must_use]
    pub fn get(&self, id: I) -> Option<&T> {
        self.values.get(id.index())
    }

    /// Gets a mutable value by typed ID.
    pub fn get_mut(&mut self, id: I) -> Option<&mut T> {
        self.values.get_mut(id.index())
    }

    /// Iterates in allocation order with typed IDs.
    #[must_use]
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (I, &T)> {
        self.values
            .iter()
            .enumerate()
            .map(|(index, value)| (I::from_raw(raw_index(index)), value))
    }

    /// Iterates mutably in allocation order with typed IDs.
    pub fn iter_mut(&mut self) -> impl ExactSizeIterator<Item = (I, &mut T)> {
        self.values
            .iter_mut()
            .enumerate()
            .map(|(index, value)| (I::from_raw(raw_index(index)), value))
    }

    /// Iterates over values without IDs.
    #[must_use]
    pub fn values(&self) -> impl ExactSizeIterator<Item = &T> {
        self.values.iter()
    }
}

impl<I: ArenaId, T> Index<I> for Arena<I, T> {
    type Output = T;

    fn index(&self, id: I) -> &Self::Output {
        &self.values[id.index()]
    }
}

impl<I: ArenaId, T> IndexMut<I> for Arena<I, T> {
    fn index_mut(&mut self, id: I) -> &mut Self::Output {
        &mut self.values[id.index()]
    }
}

/// A dense side table keyed by an arena ID.
#[derive(Clone, Deserialize, Serialize)]
pub struct ArenaMap<I, T> {
    values: Vec<Option<T>>,
    marker: PhantomData<fn(I) -> I>,
}

impl<I, T> Default for ArenaMap<I, T> {
    fn default() -> Self {
        Self {
            values: Vec::new(),
            marker: PhantomData,
        }
    }
}

impl<I, T> fmt::Debug for ArenaMap<I, T>
where
    I: ArenaId,
    T: fmt::Debug,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_map().entries(self.iter()).finish()
    }
}

impl<I: ArenaId, T> ArenaMap<I, T> {
    /// Inserts a value, returning the previous value if present.
    pub fn insert(&mut self, id: I, value: T) -> Option<T> {
        let required_len = id.index() + 1;
        if self.values.len() < required_len {
            self.values.resize_with(required_len, || None);
        }
        self.values[id.index()].replace(value)
    }

    /// Gets a value.
    #[must_use]
    pub fn get(&self, id: I) -> Option<&T> {
        self.values.get(id.index()).and_then(Option::as_ref)
    }

    /// Gets a mutable value.
    pub fn get_mut(&mut self, id: I) -> Option<&mut T> {
        self.values.get_mut(id.index()).and_then(Option::as_mut)
    }

    /// Removes a value.
    pub fn remove(&mut self, id: I) -> Option<T> {
        self.values.get_mut(id.index()).and_then(Option::take)
    }

    /// Iterates over populated entries.
    pub fn iter(&self) -> impl Iterator<Item = (I, &T)> {
        self.values.iter().enumerate().filter_map(|(index, value)| {
            value
                .as_ref()
                .map(|value| (I::from_raw(raw_index(index)), value))
        })
    }

    /// Iterates over populated values without their IDs.
    pub fn values(&self) -> impl Iterator<Item = &T> {
        self.values.iter().filter_map(Option::as_ref)
    }

    /// Returns whether the table contains no populated entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.iter().all(Option::is_none)
    }
}

fn raw_index(index: usize) -> u32 {
    u32::try_from(index).expect("arena index exceeds u32::MAX")
}

#[cfg(test)]
mod tests {
    use super::{Arena, ArenaMap};
    use crate::ExprId;

    #[test]
    fn arena_and_side_table_keep_typed_indices_aligned() {
        let mut arena = Arena::<ExprId, _>::default();
        let first = arena.alloc("first");
        let second = arena.alloc("second");

        assert_eq!(first.raw(), 0);
        assert_eq!(second.raw(), 1);
        assert_eq!(arena[second], "second");

        let mut map = ArenaMap::<ExprId, _>::default();
        assert_eq!(map.insert(second, 42), None);
        assert_eq!(map.get(first), None);
        assert_eq!(map.get(second), Some(&42));
    }
}
