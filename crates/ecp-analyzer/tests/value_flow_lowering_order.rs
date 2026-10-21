//! The AST budget is global: the queried file and the files its imports name
//! are lowered before unrelated files spend it. Covers every language the
//! flow engine parses (JavaScript, TypeScript, Python, PHP).

use ecp_analyzer::flow::{
    analyze, Budgets, Direction, FlowReport, FlowRequest, SourceFile, Subject,
};

fn source(path: &str, text: &str) -> SourceFile {
    SourceFile {
        path: path.into(),
        source: text.into(),
    }
}

/// Forward slice from "101" in `main` under an AST budget the filler file
/// alone exceeds.
fn query(files: &[SourceFile], main: &str) -> FlowReport {
    let text = &files.iter().find(|item| item.path == main).unwrap().source;
    let offset = text.find("101").expect("unique source marker");
    let prefix = &text[..offset];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let column = prefix.rsplit('\n').next().unwrap().len() + 1;
    analyze(
        files,
        &FlowRequest {
            file: main.into(),
            line,
            column,
            subject: Subject::Value,
            direction: Direction::Forward,
            budgets: Budgets {
                max_steps: 300,
                ..Budgets::default()
            },
        },
    )
    .unwrap_or_else(|error| panic!("{main}: {error}"))
}

#[test]
fn test_analyze_imported_file_lowered_before_filler_reaches_callee() {
    for (main, main_text, lib, lib_text, filler, filler_text, parameter) in [
        (
            "main.js",
            "import { identity } from './lib.js';\nconsume(identity(101));\n".to_string(),
            "lib.js",
            "export function identity(value) { return value; }\n".to_string(),
            "filler.js",
            "0;\n".repeat(400),
            "value",
        ),
        (
            "main.ts",
            "import { identity } from './lib';\nconsume(identity(101));\n".to_string(),
            "lib.ts",
            "export function identity(value: number): number { return value; }\n".to_string(),
            "filler.ts",
            "0;\n".repeat(400),
            "value",
        ),
        (
            "main.py",
            "from .lib import identity\nconsume(identity(101))\n".to_string(),
            "lib.py",
            "def identity(value):\n    return value\n".to_string(),
            "filler.py",
            "0\n".repeat(400),
            "value",
        ),
        (
            "main.php",
            "<?php\nuse App\\Lib;\nconsume(Lib::id(101));\n".to_string(),
            "lib.php",
            "<?php\nnamespace App;\nclass Lib\n{\n    public static function id($value) { return $value; }\n}\n".to_string(),
            "filler.php",
            format!("<?php\n{}", "0;\n".repeat(400)),
            "$value",
        ),
    ] {
        // Corpus order puts the filler between the query and its callee, so
        // lowering in corpus order spends the whole budget on the filler.
        let files = [
            source(main, &main_text),
            source(filler, &filler_text),
            source(lib, &lib_text),
        ];
        let report = query(&files, main);
        assert!(
            report
                .nodes
                .iter()
                .any(|n| n.file == lib && n.kind == "binding" && n.label == parameter),
            "{main}: the callee parameter in {lib} must be reached: {report:#?}"
        );
        assert!(report.truncated, "{main}: {filler} must still be cut");
        assert!(
            report.boundaries_omitted >= 1,
            "{main}: the cut in the unreached {filler} stays counted: {report:#?}"
        );
    }
}

#[test]
fn test_analyze_missing_import_target_keeps_import_boundary() {
    for (main, main_text, filler, filler_text) in [
        (
            "main.js",
            "import { thing } from './missing.js';\nconsume(101);\n".to_string(),
            "filler.js",
            "0;\n".repeat(400),
        ),
        (
            "main.ts",
            "import { thing } from './missing';\nconsume(101);\n".to_string(),
            "filler.ts",
            "0;\n".repeat(400),
        ),
        (
            "main.py",
            "from .missing import thing\nconsume(101)\n".to_string(),
            "filler.py",
            "0\n".repeat(400),
        ),
        (
            "main.php",
            "<?php\nuse App\\Missing;\nconsume(101);\n".to_string(),
            "filler.php",
            format!("<?php\n{}", "0;\n".repeat(400)),
        ),
    ] {
        let files = [source(main, &main_text), source(filler, &filler_text)];
        let report = query(&files, main);
        assert!(
            report
                .boundaries
                .iter()
                .any(|b| b.file == main && b.kind == "import"),
            "{main}: {:?}",
            report.boundaries
        );
        assert!(
            report
                .nodes
                .iter()
                .any(|n| n.kind == "argument" && n.label == "101"),
            "{main}: {report:#?}"
        );
    }
}
