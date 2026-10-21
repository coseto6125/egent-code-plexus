use super::classes::PARENT;
use super::engine::{Engine, Scope, Value};
use std::collections::BTreeSet;
use tree_sitter::Node;

const CLASS_KINDS: [&str; 4] = [
    "class_declaration",
    "interface_declaration",
    "trait_declaration",
    "enum_declaration",
];

pub(super) fn is_php(path: &str) -> bool {
    path.ends_with(".php") || path.ends_with(".phtml")
}
fn qualify(namespace: &str, name: &str) -> String {
    let namespace = namespace.trim_start_matches('\\');
    let name = name.trim_start_matches('\\');
    if namespace.is_empty() {
        name.to_string()
    } else {
        format!("{namespace}\\{name}")
    }
}
/// `use function` / `use const`: the keyword is anonymous, so only the text shows it.
fn imports_member(declaration: &str) -> bool {
    declaration
        .split_whitespace()
        .nth(1)
        .is_some_and(|keyword| matches!(keyword.to_lowercase().as_str(), "function" | "const"))
}
/// Lowercased qualified names of the classes a parse tree declares at its top
/// level or in a braced namespace. Read before lowering, like
/// [`used_names`], so the files a query imports are lowered first.
pub(super) fn declared_names(root: Node<'_>, source: &str) -> Vec<String> {
    let text = |n: Node<'_>| &source[n.byte_range()];
    let mut names = Vec::new();
    let mut namespace = "";
    let mut cursor = root.walk();
    for c in root.named_children(&mut cursor) {
        let name = c.child_by_field_name("name").map(text).unwrap_or_default();
        if c.kind() == "namespace_definition" {
            match c.child_by_field_name("body") {
                Some(body) => {
                    let mut inner = body.walk();
                    for d in body.named_children(&mut inner) {
                        if let Some(class) = d
                            .child_by_field_name("name")
                            .filter(|_| CLASS_KINDS.contains(&d.kind()))
                        {
                            names.push(qualify(name, text(class)).to_lowercase());
                        }
                    }
                }
                None => namespace = name,
            }
        } else if CLASS_KINDS.contains(&c.kind()) && !name.is_empty() {
            names.push(qualify(namespace, name).to_lowercase());
        }
    }
    names
}
/// Lowercased qualified names the `use` clauses of a parse tree import.
pub(super) fn used_names(root: Node<'_>, source: &str) -> Vec<String> {
    let text = |n: Node<'_>| &source[n.byte_range()];
    let mut statements = Vec::new();
    let mut cursor = root.walk();
    for c in root.named_children(&mut cursor) {
        match c.child_by_field_name("body") {
            Some(body) if c.kind() == "namespace_definition" => {
                let mut inner = body.walk();
                statements.extend(body.named_children(&mut inner));
            }
            _ => statements.push(c),
        }
    }
    let mut names = Vec::new();
    for n in statements {
        if n.kind() != "namespace_use_declaration" || imports_member(text(n)) {
            continue;
        }
        let mut inner = n.walk();
        let children: Vec<Node<'_>> = n.named_children(&mut inner).collect();
        let prefix = children
            .iter()
            .find(|c| c.kind() == "namespace_name")
            .map(|c| text(*c))
            .unwrap_or_default();
        for c in &children {
            let mut group = c.walk();
            let clauses: Vec<Node<'_>> = if c.kind() == "namespace_use_group" {
                c.named_children(&mut group).collect()
            } else {
                vec![*c]
            };
            for clause in clauses
                .iter()
                .filter(|c| c.kind() == "namespace_use_clause")
            {
                let mut parts = clause.walk();
                let target = clause
                    .named_children(&mut parts)
                    .find(|t| matches!(t.kind(), "name" | "qualified_name"));
                if let Some(target) = target {
                    names.push(qualify(prefix, text(target)).to_lowercase());
                }
            }
        }
    }
    names
}

impl Engine<'_> {
    pub(super) fn is_class(&self, n: usize) -> bool {
        CLASS_KINDS.contains(&self.ast[n].kind.as_str())
            && is_php(&self.files[self.ast[n].file].path)
    }
    /// Allocate every class a file declares at its top level and register its
    /// fully qualified name. Classes are autoloaded, not initialised in file
    /// order, so `use` must resolve before any file is evaluated.
    pub(super) fn php_declare(&mut self, root: usize, file: usize) {
        let mut namespace = String::new();
        for c in self.ast[root].children.clone() {
            if self.ast[c].kind == "namespace_definition" {
                let name = self.namespace_name(c);
                match self.field(c, "body") {
                    Some(body) => {
                        for d in self.ast[body].children.clone() {
                            if self.is_class(d) {
                                self.php_allocate(d, file, Some(&name));
                            }
                        }
                    }
                    None => namespace = name,
                }
            } else if self.is_class(c) {
                self.php_allocate(c, file, Some(&namespace));
            }
        }
    }
    fn namespace_name(&self, n: usize) -> String {
        self.field(n, "name")
            .map(|name| self.ast[name].text.clone())
            .unwrap_or_default()
    }
    fn php_allocate(&mut self, n: usize, scope: usize, namespace: Option<&str>) -> usize {
        let object = self.objects.len();
        self.objects.push(Scope::new());
        self.classes.insert(n, object);
        if let Some(name) = self.field(n, "name") {
            let name = self.ast[name].text.clone();
            match namespace {
                // Not a scope name: a file can hold several namespace blocks,
                // and each reference resolves against its own.
                Some(namespace) => {
                    self.declarations
                        .entry(qualify(namespace, &name).to_lowercase())
                        .or_default()
                        .insert(n);
                }
                None => {
                    self.scopes[scope].insert(
                        name,
                        Value {
                            objects: BTreeSet::from([object]),
                            ..Value::default()
                        },
                    );
                }
            }
        }
        object
    }
    /// Bind each `use` clause of a file to the classes it names.
    pub(super) fn php_imports(&mut self, root: usize, file: usize) {
        // `use` is legal only at the top level or directly in a braced namespace.
        let uses: Vec<usize> = self.ast[root]
            .children
            .iter()
            .flat_map(|c| match self.field(*c, "body") {
                Some(body) if self.ast[*c].kind == "namespace_definition" => {
                    self.ast[body].children.clone()
                }
                _ => vec![*c],
            })
            .filter(|c| self.ast[*c].kind == "namespace_use_declaration")
            .collect();
        let missing = if self.truncated {
            "The imported class is not declared in the analyzed sources, or an analysis budget cut its declaration."
        } else {
            "The imported class is not declared in the analyzed sources."
        };
        for n in uses {
            if imports_member(&self.ast[n].text) {
                self.boundary(
                    n,
                    "import",
                    "PHP function and constant imports are not resolved.",
                );
                continue;
            }
            let namespace = self.php_namespace(n).to_lowercase();
            let prefix = self.ast[n]
                .children
                .iter()
                .find(|c| self.ast[**c].kind == "namespace_name")
                .map(|c| self.ast[*c].text.clone())
                .unwrap_or_default();
            let clauses: Vec<usize> = self.ast[n]
                .children
                .iter()
                .flat_map(|c| {
                    if self.ast[*c].kind == "namespace_use_group" {
                        self.ast[*c].children.clone()
                    } else {
                        vec![*c]
                    }
                })
                .filter(|c| self.ast[*c].kind == "namespace_use_clause")
                .collect();
            for c in clauses {
                let Some(target) = self.ast[c]
                    .children
                    .iter()
                    .copied()
                    .find(|t| matches!(self.ast[*t].kind.as_str(), "name" | "qualified_name"))
                else {
                    continue;
                };
                let path = qualify(&prefix, &self.ast[target].text);
                let alias = match self.field(c, "alias") {
                    Some(alias) => self.ast[alias].text.to_lowercase(),
                    None => path.rsplit('\\').next().unwrap_or(&path).to_lowercase(),
                };
                let value = self.php_declared(&path);
                if value.objects.is_empty() {
                    self.boundary(c, "import", missing);
                    continue;
                }
                for d in self.declarations[&path.to_lowercase()].clone() {
                    let target = self.ast[d].file;
                    if target != file {
                        self.links.insert((file, target));
                    }
                }
                self.php_aliases
                    .entry((file, namespace.clone()))
                    .or_default()
                    .insert(alias, value);
            }
        }
    }
    fn php_declared(&self, qualified: &str) -> Value {
        let mut value = Value::default();
        if let Some(declarations) = self.declarations.get(&qualified.to_lowercase()) {
            value
                .objects
                .extend(declarations.iter().map(|d| self.classes[d]));
        }
        value
    }
    /// The namespace enclosing `n`: a braced `namespace X { }` around it, else
    /// the last `namespace X;` statement before it.
    fn php_namespace(&self, n: usize) -> String {
        let mut top = n;
        while let Some(parent) = self.ast[top].parent {
            if self.ast[parent].kind == "namespace_definition" {
                return self.namespace_name(parent);
            }
            if self.ast[parent].parent.is_none() {
                break;
            }
            top = parent;
        }
        let Some(root) = self.ast[top].parent else {
            return String::new();
        };
        self.ast[root]
            .children
            .iter()
            .take_while(|c| **c != top)
            .filter(|c| self.ast[**c].kind == "namespace_definition")
            .last()
            .map(|c| self.namespace_name(*c))
            .unwrap_or_default()
    }
    /// The classes a class reference (`Foo`, `Sub\Foo`, `\Ns\Foo`) names.
    pub(super) fn php_class_ref(&self, n: usize, scopes: &[usize]) -> Value {
        let text = &self.ast[n].text;
        if let Some(qualified) = text.strip_prefix('\\') {
            return self.php_declared(qualified);
        }
        let namespace = self.php_namespace(n);
        if self.ast[n].kind == "name" {
            // A class declared inside a function body is bound where it runs.
            let bound = self.lookup(text, scopes);
            if !bound.objects.is_empty() {
                return bound;
            }
            if let Some(imported) = self
                .php_aliases
                .get(&(self.ast[n].file, namespace.to_lowercase()))
                .and_then(|aliases| aliases.get(&text.to_lowercase()))
            {
                return imported.clone();
            }
        }
        // A name no `use` binds resolves against the enclosing namespace.
        self.php_declared(&qualify(&namespace, text))
    }
    /// Create the methods of a class declaration once, after the functions of
    /// its file are bound, so each method captures them.
    pub(super) fn php_class(&mut self, n: usize, scopes: &[usize]) {
        let object = match self.classes.get(&n).copied() {
            Some(object) => object,
            None => self.php_allocate(n, *scopes.last().unwrap(), None),
        };
        if !self.objects[object].is_empty() {
            return;
        }
        let parent = self.ast[n]
            .children
            .iter()
            .find(|c| self.ast[**c].kind == "base_clause")
            .and_then(|b| self.ast[*b].children.first().copied())
            .map(|p| self.php_class_ref(p, scopes))
            .unwrap_or_default();
        if !parent.objects.is_empty() {
            self.objects[object].insert(PARENT.into(), parent.clone());
        }
        let class = Value {
            objects: BTreeSet::from([object]),
            ..Value::default()
        };
        // `self`, `static` and `parent` are reserved words, so these keys never
        // collide with a user name in the captured scope.
        let scope = self.scopes.len();
        self.scopes.push(Scope::from([
            ("self".into(), class.clone()),
            ("static".into(), class),
            ("parent".into(), parent),
        ]));
        let mut class_scopes = scopes.to_vec();
        class_scopes.push(scope);
        let Some(body) = self.field(n, "body") else {
            return;
        };
        for c in self.ast[body].children.clone() {
            let kind = self.ast[c].kind.clone();
            match kind.as_str() {
                "method_declaration" => {
                    if let Some(name) = self.field(c, "name") {
                        let key = self.ast[name].text.to_lowercase();
                        let method = self.function(c, &class_scopes);
                        self.objects[object].insert(key, method);
                    }
                }
                "use_declaration" => self.boundary(
                    c,
                    "trait_use",
                    "Trait methods are not merged into the using class.",
                ),
                // Property and constant reads report their own boundary.
                _ => {}
            }
        }
    }
    /// Callees of a PHP method call. An unresolved receiver still reaches the
    /// call node, like a JS member call target does.
    pub(super) fn php_method_target(&mut self, n: usize, scopes: &[usize]) -> Value {
        let (classes, late) = if let Some(scope) = self.field(n, "scope") {
            let kind = self.ast[scope].kind.clone();
            match kind.as_str() {
                "relative_scope" => {
                    let keyword = self.ast[scope].text.to_lowercase();
                    (self.lookup(&keyword, scopes), keyword == "static")
                }
                "name" | "qualified_name" => (self.php_class_ref(scope, scopes), false),
                _ => {
                    let receiver = self.eval(scope, scopes);
                    return Value {
                        ids: receiver.ids,
                        ..Value::default()
                    };
                }
            }
        } else {
            match self.field(n, "object") {
                Some(object)
                    if self.ast[object].kind == "variable_name"
                        && self.ast[object].text == "$this" =>
                {
                    (self.lookup("static", scopes), true)
                }
                Some(object) => {
                    let receiver = self.eval(object, scopes);
                    return Value {
                        ids: receiver.ids,
                        ..Value::default()
                    };
                }
                None => return Value::default(),
            }
        };
        let Some(name) = self
            .field(n, "name")
            .filter(|name| self.ast[*name].kind == "name")
        else {
            return Value::default();
        };
        let key = self.ast[name].text.to_lowercase();
        self.class_callees(&classes.objects, &key, late)
    }
}
