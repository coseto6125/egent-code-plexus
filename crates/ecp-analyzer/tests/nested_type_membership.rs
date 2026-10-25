//! A nested type that shares its name with a type of the same file
//! (`Outer.Employee` beside `Employee`) owns its own members. On HEAD the
//! class membership pass found the nested class by span, then looked its
//! node up by name, which returned the file's first `Employee`: the
//! member edge of the nested class started at the other class.
//!
//! Each fixture holds the two `Employee` declarations on the only two
//! lines that name `Employee`, the top-level one first. Go, PHP, Dart and
//! C have no nested type that owns a method, so they have no row here.

mod overrides_support;

use ecp_analyzer::c_sharp::parser::CSharpProvider;
use ecp_analyzer::cpp::parser::CppProvider;
use ecp_analyzer::java::parser::JavaProvider;
use ecp_analyzer::javascript::parser::JavaScriptProvider;
use ecp_analyzer::kotlin::parser::KotlinProvider;
use ecp_analyzer::python::parser::PythonProvider;
use ecp_analyzer::ruby::parser::RubyProvider;
use ecp_analyzer::rust::parser::RustProvider;
use ecp_analyzer::swift::parser::SwiftProvider;
use ecp_analyzer::typescript::parser::TypeScriptProvider;
use ecp_core::analyzer::LanguageProvider;
use ecp_core::graph::{NodeKind, RelType};
use overrides_support::edge_ends;

/// The 0-based rows of the lines that name `Employee`.
fn employee_rows(source: &str) -> Vec<u32> {
    source
        .lines()
        .enumerate()
        .filter(|(_, line)| line.contains("Employee"))
        .map(|(row, _)| row as u32)
        .collect()
}

/// Asserts that `first` belongs to the top-level `Employee` and `second`
/// to the nested one: each member's `HasMethod` source is the class
/// declared on that row.
fn assert_own_members(provider: &dyn LanguageProvider, path: &str, source: &str) {
    let graph = overrides_support::graph_of(provider, &[(path, source)]);
    let rows = employee_rows(source);
    assert_eq!(rows.len(), 2, "fixture names `Employee` on two lines");
    let classes: Vec<u32> = graph
        .nodes
        .iter()
        .filter(|n| {
            matches!(n.kind, NodeKind::Class | NodeKind::Struct)
                && n.name.resolve(graph.string_pool.as_slice()) == "Employee"
        })
        .map(|n| n.span.0)
        .collect();
    assert_eq!(
        classes, rows,
        "{path}: one live node per `Employee` declaration"
    );
    let owner_rows = |member: &str| -> Vec<u32> {
        let mut found: Vec<u32> = edge_ends(&graph, RelType::HasMethod)
            .into_iter()
            .filter(|(_, target)| target.name.eq_ignore_ascii_case(member))
            .map(|(source, _)| source.line)
            .collect();
        found.sort_unstable();
        found.dedup();
        found
    };
    assert_eq!(owner_rows("first"), vec![rows[0]], "{path}: owner of first");
    assert_eq!(
        owner_rows("second"),
        vec![rows[1]],
        "{path}: owner of second"
    );
}

#[test]
fn test_class_membership_csharp_nested_same_name_type_owns_its_members() {
    let src = "namespace App {
    public class Employee {
        public void First() {}
    }

    public class Outer {
        public class Employee {
            public void Second() {}
        }
    }
}
";
    assert_own_members(&CSharpProvider::new().unwrap(), "App/Staff.cs", src);
}

#[test]
fn test_class_membership_java_nested_same_name_type_owns_its_members() {
    let src = "class Employee {
    void first() {}
}

class Outer {
    static class Employee {
        void second() {}
    }
}
";
    assert_own_members(&JavaProvider::new().unwrap(), "app/Staff.java", src);
}

#[test]
fn test_class_membership_kotlin_nested_same_name_type_owns_its_members() {
    let src = "class Employee {
    fun first() {}
}

class Outer {
    class Employee {
        fun second() {}
    }
}
";
    assert_own_members(&KotlinProvider::new().unwrap(), "app/Staff.kt", src);
}

#[test]
fn test_class_membership_python_nested_same_name_type_owns_its_members() {
    let src = "class Employee:
    def first(self):
        pass


class Outer:
    class Employee:
        def second(self):
            pass
";
    assert_own_members(&PythonProvider::new().unwrap(), "app/staff.py", src);
}

#[test]
fn test_class_membership_typescript_nested_same_name_type_owns_its_members() {
    let src = "class Employee {
  first() {}
}

namespace Outer {
  export class Employee {
    second() {}
  }
}
";
    assert_own_members(&TypeScriptProvider::new().unwrap(), "src/staff.ts", src);
}

#[test]
fn test_class_membership_javascript_nested_same_name_type_owns_its_members() {
    let src = "class Employee {
  first() {}
}

class Outer {
  static Inner = class Employee {
    second() {}
  };
}
";
    assert_own_members(&JavaScriptProvider::new().unwrap(), "src/staff.js", src);
}

#[test]
fn test_class_membership_ruby_nested_same_name_type_owns_its_members() {
    let src = "class Employee
  def first
  end
end

class Outer
  class Employee
    def second
    end
  end
end
";
    assert_own_members(&RubyProvider::new().unwrap(), "app/staff.rb", src);
}

#[test]
fn test_class_membership_swift_nested_same_name_type_owns_its_members() {
    let src = "class Employee {
    func first() {}
}

class Outer {
    class Employee {
        func second() {}
    }
}
";
    assert_own_members(&SwiftProvider::new().unwrap(), "App/Staff.swift", src);
}

#[test]
fn test_class_membership_cpp_nested_same_name_type_owns_its_members() {
    let src = "class Employee {
    void first() {}
};

class Outer {
    class Employee {
        void second() {}
    };
};
";
    assert_own_members(&CppProvider::new().unwrap(), "src/staff.cpp", src);
}

#[test]
fn test_class_membership_rust_nested_same_name_type_owns_its_members() {
    let src = "struct Employee;
impl Employee { fn first(&self) {} }

mod outer {
    pub struct Employee;
    impl Employee { fn second(&self) {} }
}
";
    // Each impl block names `Employee` too, so the struct rows are fixed.
    let graph =
        overrides_support::graph_of(&RustProvider::new().unwrap(), &[("src/staff.rs", src)]);
    let owner_of = |member: &str| -> Vec<u32> {
        let mut rows: Vec<u32> = edge_ends(&graph, RelType::HasMethod)
            .into_iter()
            .filter(|(_, target)| target.name == member)
            .map(|(source, _)| source.line)
            .collect();
        rows.sort_unstable();
        rows.dedup();
        rows
    };
    assert_eq!(owner_of("first"), vec![0], "owner of first");
    assert_eq!(owner_of("second"), vec![4], "owner of second");
}
