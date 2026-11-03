//! FU-2026-11-01-c07a40e90e36, dynamic module load: C#
//! `Assembly.LoadFrom(..).GetType(..).GetMethod(..).Invoke`.
//!
//! Load form: `Assembly.LoadFrom("x.dll").GetType("P.X").GetMethod("Run").Invoke(null, null)`.
//! The type and method names are strings, so nothing names a declaration; the
//! calls the parser sees are `LoadFrom`, `GetType`, `GetMethod` and `Invoke`,
//! none of which the project declares. Expected: no edge to the project `X.Run`.
//!
//! Contract the pins stand for: a runtime load with a non-static target
//! gives no import-tier call edge; a future fix that adds one with a proven
//! target is a change, not a regression. Each file declares a decoy with the
//! loaded name, so a unique-name fallback cannot bind it by accident. Every
//! "no edge" pin has a control test that proves the harness sees a plain
//! cross-file call in the same language.
use crate::instantiation_calls_support;

use ecp_analyzer::c_sharp::parser::CSharpProvider;
use ecp_core::graph::{RelType, ZeroCopyGraph};
use instantiation_calls_support::{calls_from, edges_from, graph_of};

fn graph(files: &[(&str, &str)]) -> ZeroCopyGraph {
    graph_of(&CSharpProvider::new().expect("CSharpProvider::new"), files)
}

/// `owner.name` of every `Calls` edge from `caller` into `file`.
fn calls_into(graph: &ZeroCopyGraph, caller: &str, file: &str) -> Vec<String> {
    calls_from(graph, caller)
        .into_iter()
        .filter(|hit| hit.file == file)
        .map(|hit| format!("{}.{}", hit.owner, hit.name))
        .collect()
}

/// Target files of every `Imports` edge of the File node named `file`.
#[allow(dead_code)]
fn imports_of(graph: &ZeroCopyGraph, file: &str) -> Vec<String> {
    edges_from(graph, file, RelType::Imports)
        .into_iter()
        .map(|hit| hit.file)
        .collect()
}

const DECOY: (&str, &str) = (
    "X.cs",
    "namespace P { public class X { public static void Run() {} public static void Plain() {} } }",
);

const LOADER: (&str, &str) = (
    "Loader.cs",
    r#"using System.Reflection;
namespace P {
    public class Loader {
        public void Load() {
            Assembly.LoadFrom("x.dll").GetType("P.X").GetMethod("Run").Invoke(null, null);
        }
    }
}
"#,
);

#[test]
fn test_assembly_loadfrom_reflection_invoke_binds_no_project_run() {
    let g = graph(&[DECOY, LOADER]);
    assert_eq!(
        calls_into(&g, "Load", DECOY.0),
        Vec::<String>::new(),
        "Load calls: {:?}",
        calls_from(&g, "Load")
    );
}

/// Control: a static call to a project type binds.
#[test]
fn test_assembly_loadfrom_control_plain_static_call_binds() {
    let g = graph(&[
        DECOY,
        (
            "Ctl.cs",
            "namespace P { public class Ctl { public void Control() { X.Plain(); } } }",
        ),
    ]);
    assert_eq!(
        calls_into(&g, "Control", DECOY.0),
        vec!["X.Plain".to_string()]
    );
}
