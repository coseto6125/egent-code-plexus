use super::engine::{Engine, Value};
use std::collections::{BTreeMap, BTreeSet};

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
    /// The member `key` of a class: the first class in its method resolution
    /// order that defines it. When the order is unknown (a base that resolves
    /// to several classes, or an inconsistent hierarchy), every nearest
    /// definition along each base path is a possible member.
    pub(super) fn inherited(&self, class: usize, key: &str, members: &mut Value) {
        // A class's own member comes first in every resolution order, and an
        // object without bases (most field reads) needs no order at all.
        if let Some(member) = self.objects[class].get(key) {
            members.merge(member);
            return;
        }
        if !self.bases.contains_key(&class) && !self.objects[class].contains_key(PARENT) {
            return;
        }
        if let Some(order) = self.linearize(class, &mut Vec::new(), &mut BTreeMap::new()) {
            if let Some(member) = order.iter().skip(1).find_map(|c| self.objects[*c].get(key)) {
                members.merge(member);
            }
            return;
        }
        let mut seen = BTreeSet::new();
        let mut pending = vec![class];
        while let Some(c) = pending.pop() {
            if !seen.insert(c) {
                continue;
            }
            if let Some(member) = self.objects[c].get(key) {
                members.merge(member);
            } else if let Some(parent) = self.objects[c].get(PARENT) {
                pending.extend(&parent.objects);
            }
        }
    }
    /// The bases of a class in declared order, one set of possible classes
    /// per base. A PHP class has at most one.
    fn base_slots(&self, class: usize) -> Vec<BTreeSet<usize>> {
        match self.bases.get(&class) {
            Some(slots) => slots.clone(),
            None => self.objects[class]
                .get(PARENT)
                .map(|parent| vec![parent.objects.clone()])
                .unwrap_or_default(),
        }
    }
    /// C3 linearization (Python's MRO; the ancestor chain for single
    /// inheritance). `None` when a base is not exactly one class, the
    /// hierarchy is cyclic, or no consistent order exists. `done` holds the
    /// orders this lookup already computed: stacked diamonds reach each
    /// ancestor along exponentially many paths.
    fn linearize(
        &self,
        class: usize,
        visiting: &mut Vec<usize>,
        done: &mut BTreeMap<usize, Option<Vec<usize>>>,
    ) -> Option<Vec<usize>> {
        if let Some(order) = done.get(&class) {
            return order.clone();
        }
        if visiting.contains(&class) {
            return None;
        }
        visiting.push(class);
        let order = self.c3(class, visiting, done);
        visiting.pop();
        done.insert(class, order.clone());
        order
    }
    fn c3(
        &self,
        class: usize,
        visiting: &mut Vec<usize>,
        done: &mut BTreeMap<usize, Option<Vec<usize>>>,
    ) -> Option<Vec<usize>> {
        let bases = self
            .base_slots(class)
            .into_iter()
            .map(|slot| match slot.len() {
                1 => slot.first().copied(),
                _ => None,
            })
            .collect::<Option<Vec<usize>>>()?;
        let mut sequences = bases
            .iter()
            .map(|base| self.linearize(*base, visiting, done))
            .collect::<Option<Vec<Vec<usize>>>>()?;
        sequences.push(bases);
        let mut order = vec![class];
        while sequences.iter().any(|s| !s.is_empty()) {
            let head = sequences
                .iter()
                .filter_map(|s| s.first())
                .find(|head| {
                    sequences
                        .iter()
                        .all(|s| !s.iter().skip(1).any(|c| c == *head))
                })
                .copied()?;
            order.push(head);
            for s in &mut sequences {
                if s.first() == Some(&head) {
                    s.remove(0);
                }
            }
        }
        Some(order)
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
