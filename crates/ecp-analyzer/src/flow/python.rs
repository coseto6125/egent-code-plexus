use super::classes::PARENT;
use super::engine::{Engine, Scope, Value};
use std::collections::BTreeSet;

pub(super) fn is_python(path: &str) -> bool {
    path.ends_with(".py") || path.ends_with(".pyi")
}

/// What a call through a class or `self` passes ahead of a Python method's
/// arguments, as its decorators decide.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Receiver {
    /// A plain method: `self.m(x)` passes the instance, `Cls.m(x)` nothing.
    Instance,
    /// `@staticmethod`: nothing, however it is reached.
    Static,
    /// `@classmethod`: the class, however it is reached.
    Class,
    /// Another decorator replaced the function; its call semantics are unknown.
    Unknown,
}

impl Engine<'_> {
    /// Execute a class statement. Its body runs in a class scope, so what it
    /// binds becomes the class object's attributes instead of names in the
    /// enclosing scope; functions defined there do not see that scope.
    pub(super) fn python_class(&mut self, n: usize, scopes: &[usize]) {
        // Each positional base separately: the resolution order needs the
        // declared order, which the merged PARENT set drops.
        let mut slots = Vec::new();
        if let Some(superclasses) = self.field(n, "superclasses") {
            for c in self.ast[superclasses].children.clone() {
                let base = self.eval(c, scopes);
                if !matches!(self.ast[c].kind.as_str(), "keyword_argument" | "comment") {
                    slots.push(base.objects);
                }
            }
        }
        let bases: BTreeSet<usize> = slots.iter().flatten().copied().collect();
        // One object per class statement, so a class statement in a loop converges.
        let object = match self.classes.get(&n).copied() {
            Some(object) => object,
            None => {
                let object = self.objects.len();
                self.objects.push(Scope::new());
                self.classes.insert(n, object);
                object
            }
        };
        self.bases.insert(object, slots);
        let scope = self.scopes.len();
        self.scopes.push(Scope::new());
        self.class_scopes.insert(scope);
        let mut body_scopes = scopes.to_vec();
        body_scopes.push(scope);
        if let Some(body) = self.field(n, "body") {
            self.eval(body, &body_scopes);
        }
        let mut attributes = self.scopes[scope].clone();
        if !bases.is_empty() {
            attributes.insert(
                PARENT.into(),
                Value {
                    objects: bases,
                    ..Value::default()
                },
            );
        }
        self.objects[object] = attributes;
        if let Some(name) = self.field(n, "name") {
            let class = Value {
                objects: BTreeSet::from([object]),
                ..Value::default()
            };
            self.assign(name, class, scopes, true);
        }
    }
    /// For a Python method (a function defined directly in a class body), how
    /// its decorators bind the receiver, and the class statement.
    pub(super) fn python_method(&self, function: usize) -> Option<(Receiver, usize)> {
        let a = &self.ast[function];
        if a.kind != "function_definition" || !is_python(&self.files[a.file].path) {
            return None;
        }
        let mut holder = a.parent?;
        let decorated = (self.ast[holder].kind == "decorated_definition").then_some(holder);
        if let Some(decorated) = decorated {
            holder = self.ast[decorated].parent?;
        }
        let class = self.ast[holder].parent.filter(|c| {
            self.ast[holder].kind == "block" && self.ast[*c].kind == "class_definition"
        })?;
        let mut receiver = Receiver::Instance;
        for d in decorated.map_or(&[][..], |d| &self.ast[d].children[..]) {
            if self.ast[*d].kind != "decorator" {
                continue;
            }
            receiver = match (receiver, self.ast[*d].text.trim_start_matches('@').trim()) {
                // These return the function itself, so the binding is unchanged.
                (
                    _,
                    "abstractmethod"
                    | "abc.abstractmethod"
                    | "override"
                    | "typing.override"
                    | "typing_extensions.override"
                    | "final"
                    | "typing.final",
                ) => receiver,
                (Receiver::Instance, "staticmethod") => Receiver::Static,
                (Receiver::Instance, "classmethod") => Receiver::Class,
                _ => Receiver::Unknown,
            };
        }
        Some((receiver, class))
    }
    /// Whether every object `value` may hold is a class object, and it holds one.
    pub(super) fn only_classes(&self, value: &Value) -> bool {
        !value.objects.is_empty()
            && value
                .objects
                .iter()
                .all(|o| self.classes.values().any(|c| c == o))
    }
    /// Callees of `self.name(...)` where `self` is the first parameter of the
    /// enclosing plain method, plus that receiver: a bound method receives
    /// the instance, a class method the instance's class.
    pub(super) fn python_self_call(&self, target: usize) -> Option<(Value, usize)> {
        let a = &self.ast[target];
        if a.kind != "attribute" || !is_python(&self.files[a.file].path) {
            return None;
        }
        let receiver = self
            .field(target, "object")
            .filter(|r| self.ast[*r].kind == "identifier")?;
        let attribute = self.field(target, "attribute")?;
        let mut function = a.parent;
        while let Some(f) = function.filter(|f| !self.is_function(*f)) {
            function = self.ast[f].parent;
        }
        let function = function?;
        // A static or class method's first parameter is not the instance.
        let (Receiver::Instance, class) = self.python_method(function)? else {
            return None;
        };
        let first = *self.parameters(function).first()?;
        if self.ast[first].text != self.ast[receiver].text {
            return None;
        }
        let object = *self.classes.get(&class)?;
        let callees =
            self.class_callees(&BTreeSet::from([object]), &self.ast[attribute].text, true);
        (!callees.functions.is_empty()).then_some((callees, receiver))
    }
}
