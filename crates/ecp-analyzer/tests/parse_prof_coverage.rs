//! Every mainstream parser must report its tree-sitter parse and its main
//! query loop to `parse_prof`; the shared-driver slices read these timers to
//! prove a parser move costs no time.

use ecp_analyzer::c::parser::CProvider;
use ecp_analyzer::c_sharp::parser::CSharpProvider;
use ecp_analyzer::cpp::parser::CppProvider;
use ecp_analyzer::dart::parser::DartProvider;
use ecp_analyzer::go::parser::GoProvider;
use ecp_analyzer::java::parser::JavaProvider;
use ecp_analyzer::javascript::parser::JavaScriptProvider;
use ecp_analyzer::kotlin::parser::KotlinProvider;
use ecp_analyzer::php::parser::PhpProvider;
use ecp_analyzer::python::parser::PythonProvider;
use ecp_analyzer::ruby::parser::RubyProvider;
use ecp_analyzer::rust::parser::RustProvider;
use ecp_analyzer::swift::parser::SwiftProvider;
use ecp_analyzer::typescript::parser::TypeScriptProvider;
use ecp_core::analyzer::parse_prof::{self, Pass};
use ecp_core::analyzer::provider::LanguageProvider;

struct Case {
    name: &'static str,
    file: &'static str,
    source: &'static str,
    provider: Box<dyn LanguageProvider>,
}

fn case(
    name: &'static str,
    file: &'static str,
    source: &'static str,
    provider: anyhow::Result<impl LanguageProvider + 'static>,
) -> Case {
    Case {
        name,
        file,
        source,
        provider: Box::new(provider.unwrap()),
    }
}

fn cases() -> Vec<Case> {
    vec![
        case(
            "typescript",
            "a.ts",
            "import { b } from './b';\nfunction a() { b(); }\n",
            TypeScriptProvider::new(),
        ),
        case(
            "javascript",
            "a.js",
            "import { b } from './b';\nfunction a() { b(); }\n",
            JavaScriptProvider::new(),
        ),
        case(
            "python",
            "a.py",
            "import os\n\ndef a():\n    os.getcwd()\n",
            PythonProvider::new(),
        ),
        case(
            "java",
            "A.java",
            "import java.util.List;\nclass A { void a() { b(); } }\n",
            JavaProvider::new(),
        ),
        case(
            "kotlin",
            "a.kt",
            "import kotlin.io.println\nfun a() { println(1) }\n",
            KotlinProvider::new(),
        ),
        case(
            "c_sharp",
            "A.cs",
            "using System;\nclass A { void M() { Console.WriteLine(1); } }\n",
            CSharpProvider::new(),
        ),
        case(
            "go",
            "a.go",
            "package main\nimport \"fmt\"\nfunc a() { fmt.Println(1) }\n",
            GoProvider::new(),
        ),
        case(
            "rust",
            "a.rs",
            "use std::fmt;\nfn a() { b(); }\n",
            RustProvider::new(),
        ),
        case(
            "php",
            "a.php",
            "<?php\nuse Foo\\Bar;\nfunction a() { b(); }\n",
            PhpProvider::new(),
        ),
        case(
            "ruby",
            "a.rb",
            "require 'json'\ndef a\n  b()\nend\n",
            RubyProvider::new(),
        ),
        case(
            "swift",
            "a.swift",
            "import Foundation\nfunc a() { b() }\n",
            SwiftProvider::new(),
        ),
        case(
            "c",
            "a.c",
            "#include <stdio.h>\nvoid a(void) { b(); }\n",
            CProvider::new(),
        ),
        case(
            "cpp",
            "a.cpp",
            "#include <vector>\nvoid a() { b(); }\n",
            CppProvider::new(),
        ),
        case(
            "dart",
            "a.dart",
            "import 'dart:io';\nvoid a() { b(); }\n",
            DartProvider::new(),
        ),
    ]
}

#[test]
fn test_parse_prof_all_mainstream_parsers_record_tree_and_query() {
    parse_prof::set_enabled(true);
    for c in cases() {
        parse_prof::reset();
        c.provider
            .parse_file(c.file.as_ref(), c.source.as_bytes())
            .unwrap_or_else(|e| panic!("{}: parse_file failed: {e}", c.name));
        let got = parse_prof::take();
        assert!(got[Pass::Tree as usize] > 0, "{}: no Tree time", c.name);
        assert!(got[Pass::Query as usize] > 0, "{}: no Query time", c.name);

        parse_prof::reset();
        let empty = c.provider.parse_file(c.file.as_ref(), b"");
        assert!(empty.is_ok(), "{}: zero-byte file must parse", c.name);
        parse_prof::reset();
    }
    parse_prof::set_enabled(false);
}
