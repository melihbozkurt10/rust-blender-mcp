//! Category bookkeeping.

use std::collections::BTreeSet;

use blender_protocol::command::Category;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// One category as reported by `list_tool_categories`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CategoryInfo {
    pub id: &'static str,
    pub enabled: bool,
    /// Core is always on; disabling it would remove the tool that turns things
    /// back on.
    pub always_on: bool,
    pub tool_count: usize,
    pub description: &'static str,
}

/// A set of categories. Ordered so listings are stable between calls.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CategorySet {
    inner: BTreeSet<Category>,
}

impl CategorySet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn all() -> Self {
        Category::ALL.into_iter().collect()
    }

    pub fn contains(&self, category: Category) -> bool {
        self.inner.contains(&category)
    }

    /// Insert a category, returning whether it was newly added.
    pub fn insert(&mut self, category: Category) -> bool {
        self.inner.insert(category)
    }

    /// Remove a category, returning whether it was present.
    pub fn remove(&mut self, category: Category) -> bool {
        self.inner.remove(&category)
    }

    pub fn iter(&self) -> impl Iterator<Item = Category> + '_ {
        self.inner.iter().copied()
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    pub fn ids(&self) -> Vec<&'static str> {
        self.inner.iter().map(|c| c.id()).collect()
    }
}

impl FromIterator<Category> for CategorySet {
    fn from_iter<I: IntoIterator<Item = Category>>(categories: I) -> Self {
        Self {
            inner: categories.into_iter().collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sets_are_ordered_and_deduplicated() {
        let mut set = CategorySet::from_iter([Category::Render, Category::Core, Category::Core]);
        assert_eq!(set.len(), 2);
        assert!(set.contains(Category::Core));
        assert!(!set.insert(Category::Core));
        assert!(set.insert(Category::Mesh));
        // BTreeSet ordering follows the enum's declaration order, which puts
        // core first -- the order a human would expect in a listing.
        assert_eq!(set.ids()[0], "core");
    }

    #[test]
    fn all_covers_every_category() {
        assert_eq!(CategorySet::all().len(), Category::ALL.len());
    }
}
