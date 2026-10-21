use super::engine::{Engine, Value};
use std::collections::BTreeSet;

/// Class-object key of the base classes. `::` never occurs in a PHP or Python
/// member name, so the key cannot shadow a method.
pub(super) const PARENT: &str = "::parent";

impl Engine<'_> {
    /// Callees of method `key` on `classes`. A `late` call dispatches on the
    /// runtime class (`$this`, `static::`, Python `self`), so an override in
    /// any subclass is a possible callee too.
    pub(super) fn class_callees(&self, classes: &BTreeSet<usize>, key: &str, late: bool) -> Value {
        let mut callees = Value::default();
        for &class in classes {
            self.inherited(class, key, &mut callees);
            if late {
                for &sub in self.classes.values() {
                    if let Some(method) = self.objects[sub].get(key) {
                        if sub != class && self.extends(sub, class) {
                            callees.merge(method);
                        }
                    }
                }
            }
        }
        callees
    }
    /// The method `key` of a class: its own, else the nearest ancestor's.
    fn inherited(&self, class: usize, key: &str, callees: &mut Value) {
        let mut seen = BTreeSet::new();
        let mut pending = vec![class];
        while let Some(c) = pending.pop() {
            if !seen.insert(c) {
                continue;
            }
            if let Some(method) = self.objects[c].get(key) {
                callees.merge(method);
            } else if let Some(parent) = self.objects[c].get(PARENT) {
                pending.extend(&parent.objects);
            }
        }
    }
    fn extends(&self, class: usize, ancestor: usize) -> bool {
        let mut seen = BTreeSet::new();
        let mut pending = vec![class];
        while let Some(c) = pending.pop() {
            if !seen.insert(c) {
                continue;
            }
            if let Some(parent) = self.objects[c].get(PARENT) {
                if parent.objects.contains(&ancestor) {
                    return true;
                }
                pending.extend(&parent.objects);
            }
        }
        false
    }
}
