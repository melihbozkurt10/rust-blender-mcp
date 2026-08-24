//! Which categories are exposed, and how that can change.
//!
//! A hundred-odd tool schemas is a lot of context to hand a model that only
//! wants to move a cube. The default is therefore lazy: a compact core is
//! visible, and categories are switched on as they are needed.
//!
//! Some MCP clients do not refresh their tool list on
//! `notifications/tools/list_changed`. For those, eager mode registers
//! everything up front. Both modes are supported deliberately rather than one
//! being a workaround the user has to discover.

use std::sync::RwLock;

use blender_protocol::command::Category;

use super::category::CategorySet;

/// The activation policy plus the current state.
#[derive(Debug)]
pub struct Activation {
    mode: Mode,
    enabled: RwLock<CategorySet>,
}

/// How categories become visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Only enabled categories are listed; `enable_tool_category` adds more.
    Lazy,
    /// Everything is listed from the start and cannot be turned off.
    Eager,
}

impl Activation {
    /// Lazy mode, starting with the given categories. Core is always included.
    pub fn lazy(initial: &[Category]) -> Self {
        let mut set = CategorySet::from_iter(initial.iter().copied());
        set.insert(Category::Core);
        Self {
            mode: Mode::Lazy,
            enabled: RwLock::new(set),
        }
    }

    /// Eager mode: every category, permanently.
    pub fn eager() -> Self {
        Self {
            mode: Mode::Eager,
            enabled: RwLock::new(CategorySet::all()),
        }
    }

    /// Pick a mode from configuration.
    pub fn from_config(eager: bool, initial: &[Category]) -> Self {
        if eager {
            Self::eager()
        } else {
            Self::lazy(initial)
        }
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn is_eager(&self) -> bool {
        self.mode == Mode::Eager
    }

    pub fn is_enabled(&self, category: Category) -> bool {
        self.read().contains(category)
    }

    pub fn enabled(&self) -> CategorySet {
        self.read().clone()
    }

    /// Enable a category. Returns whether the visible tool list changed.
    pub fn enable(&self, category: Category) -> bool {
        if self.mode == Mode::Eager {
            return false;
        }
        self.write().insert(category)
    }

    /// Disable a category. Returns whether the visible tool list changed.
    ///
    /// Core cannot be disabled: the tools that re-enable a category live
    /// there, so turning it off would be a one-way door.
    pub fn disable(&self, category: Category) -> Result<bool, DisableRefused> {
        if category == Category::Core {
            return Err(DisableRefused::Core);
        }
        if self.mode == Mode::Eager {
            return Err(DisableRefused::Eager);
        }
        Ok(self.write().remove(category))
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, CategorySet> {
        self.enabled.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, CategorySet> {
        self.enabled.write().unwrap_or_else(|e| e.into_inner())
    }
}

/// Why a category could not be disabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DisableRefused {
    #[error("the `core` category cannot be disabled: it holds the tools that enable the others")]
    Core,
    #[error("categories cannot be disabled in eager mode (BLENDER_MCP_EAGER_TOOLS is set)")]
    Eager,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lazy_starts_with_core_even_if_not_asked_for() {
        let activation = Activation::lazy(&[]);
        assert!(activation.is_enabled(Category::Core));
        assert!(!activation.is_enabled(Category::Mesh));
    }

    #[test]
    fn enabling_reports_whether_anything_changed() {
        let activation = Activation::lazy(&[]);
        assert!(
            activation.enable(Category::Mesh),
            "first enable is a change"
        );
        assert!(
            !activation.enable(Category::Mesh),
            "second enable changes nothing"
        );
        assert!(activation.is_enabled(Category::Mesh));
    }

    #[test]
    fn core_cannot_be_disabled() {
        let activation = Activation::lazy(&[Category::Mesh]);
        assert_eq!(
            activation.disable(Category::Core),
            Err(DisableRefused::Core)
        );
        assert!(activation.is_enabled(Category::Core));
        assert_eq!(activation.disable(Category::Mesh), Ok(true));
        assert_eq!(activation.disable(Category::Mesh), Ok(false));
    }

    #[test]
    fn eager_enables_everything_and_refuses_changes() {
        let activation = Activation::eager();
        for category in Category::ALL {
            assert!(activation.is_enabled(category), "{}", category.id());
        }
        assert!(
            !activation.enable(Category::Mesh),
            "nothing to enable in eager mode"
        );
        assert_eq!(
            activation.disable(Category::Mesh),
            Err(DisableRefused::Eager)
        );
    }
}
