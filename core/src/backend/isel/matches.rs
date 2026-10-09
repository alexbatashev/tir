//! The function's value matches, by the class of the value they root.

use std::cell::RefCell;

use tir_egraph::ClassId as Id;

use super::pattern::Found;
use crate::utils::fx::HashMap;

/// The matches rooted in one class, each with its pattern.
pub(crate) type InClass = std::rc::Rc<[(usize, Found)]>;

/// The value matches of a function rooted at a row, searched a class at a
/// time when selection first reads the class: a class no instance reads is
/// never searched. A class holds rows at several offsets, so its matches root
/// at several values.
///
/// The graph does not change once saturation ends, so a class is searched
/// once.
pub(crate) struct Matches {
    /// The matches of the patterns searched over the whole function ahead of
    /// any class, by class.
    whole: HashMap<Id, Vec<(usize, Found)>>,
    /// Per class searched, its matches by ascending pattern, then in the
    /// order the search made them.
    classes: RefCell<HashMap<Id, InClass>>,
}

impl Matches {
    /// `whole` is every match of the patterns no class search covers, in
    /// the order the cover reads them.
    pub(crate) fn new(whole: Vec<(usize, Found)>) -> Self {
        let mut by_class: HashMap<Id, Vec<(usize, Found)>> = HashMap::default();
        for (pattern, found) in whole {
            by_class
                .entry(found.root.class)
                .or_default()
                .push((pattern, found));
        }
        Self {
            whole: by_class,
            classes: RefCell::default(),
        }
    }

    /// The matches rooted at any value of `class`, ascending by pattern.
    /// `search` finds the ones a class search covers, the first time the
    /// class is read.
    pub(crate) fn in_class(
        &self,
        class: Id,
        search: impl FnOnce() -> Vec<(usize, Found)>,
    ) -> InClass {
        if let Some(found) = self.classes.borrow().get(&class) {
            return found.clone();
        }
        let mut found = self.whole.get(&class).cloned().unwrap_or_default();
        found.extend(search());
        found.sort_by_key(|&(pattern, _)| pattern);
        let found: InClass = found.into();
        self.classes.borrow_mut().insert(class, found.clone());
        found
    }
}
