use super::classes::PARENT;
use super::engine::{Engine, Scope, Value};
use std::collections::BTreeSet;

impl Engine<'_> {
    /// Execute a class statement. Its body runs in a class scope, so what it
    /// binds becomes the class object's attributes instead of names in the
    /// enclosing scope; functions defined there do not see that scope.
    pub(super) fn python_class(&mut self, n: usize, scopes: &[usize]) {
        let bases = self
            .field(n, "superclasses")
            .map(|s| self.eval(s, scopes))
            .unwrap_or_default();
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
        let scope = self.scopes.len();
        self.scopes.push(Scope::new());
        self.class_scopes.insert(scope);
        let mut body_scopes = scopes.to_vec();
        body_scopes.push(scope);
        if let Some(body) = self.field(n, "body") {
            self.eval(body, &body_scopes);
        }
        let mut attributes = self.scopes[scope].clone();
        if !bases.objects.is_empty() {
            attributes.insert(
                PARENT.into(),
                Value {
                    objects: bases.objects,
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
    /// Callees of `self.name(...)` where `self` is the first parameter of the
    /// enclosing method, plus that receiver: a bound method receives it as its
    /// first argument.
    pub(super) fn python_self_call(&self, target: usize) -> Option<(Value, usize)> {
        let a = &self.ast[target];
        let path = &self.files[a.file].path;
        if a.kind != "attribute" || !(path.ends_with(".py") || path.ends_with(".pyi")) {
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
        let function = function.filter(|f| self.ast[*f].kind == "function_definition")?;
        let first = *self.parameters(function).first()?;
        if self.ast[first].text != self.ast[receiver].text {
            return None;
        }
        let mut holder = self.ast[function].parent?;
        if self.ast[holder].kind == "decorated_definition" {
            holder = self.ast[holder].parent?;
        }
        let class = self.ast[holder].parent.filter(|c| {
            self.ast[holder].kind == "block" && self.ast[*c].kind == "class_definition"
        })?;
        let object = *self.classes.get(&class)?;
        let callees =
            self.class_callees(&BTreeSet::from([object]), &self.ast[attribute].text, true);
        (!callees.functions.is_empty()).then_some((callees, receiver))
    }
}
