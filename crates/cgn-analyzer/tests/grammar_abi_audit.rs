//! ABI version + parse-perf audit across every grammar this crate links against.
//!
//! Tree-sitter v0.25.0 bumped the internal ABI to 15. Grammars compiled by older
//! generators still load (backward-compat) but skip name / supertype / reserved-words
//! metadata. This test surfaces:
//!   - which grammars are on ABI ≥ 15 vs older
//!   - per-grammar parse cost on a small representative snippet
//!
//! Marked `#[ignore]` to keep it out of default test runs. Invoke explicitly:
//!     cargo test -p cgn-analyzer --test grammar_abi_audit -- --ignored --nocapture

use std::time::Instant;
use tree_sitter::{Language, Parser};

const ITERS: usize = 500;
const WARMUP: usize = 10;

struct Row {
    display: &'static str,
    abi: usize,
    grammar_name: String,
    node_kinds: usize,
    fields: usize,
    snippet_bytes: usize,
    per_parse_us: f64,
}

fn audit(display: &'static str, lang: Language, snippet: &str) -> Row {
    let abi = lang.abi_version();
    let node_kinds = lang.node_kind_count();
    let fields = lang.field_count();
    let grammar_name = lang
        .name()
        .map_or_else(|| "<none>".to_string(), |s| s.to_string());

    let mut parser = Parser::new();
    parser.set_language(&lang).expect("set_language");

    for _ in 0..WARMUP {
        let _ = parser.parse(snippet, None);
    }

    let start = Instant::now();
    for _ in 0..ITERS {
        let _ = parser.parse(snippet, None).expect("parse");
    }
    let elapsed = start.elapsed();
    let per_parse_us = elapsed.as_nanos() as f64 / (ITERS as f64 * 1_000.0);

    Row {
        display,
        abi,
        grammar_name,
        node_kinds,
        fields,
        snippet_bytes: snippet.len(),
        per_parse_us,
    }
}

#[test]
#[ignore]
fn grammar_abi_audit() {
    let mut rows: Vec<Row> = vec![audit(
        "rust",
        tree_sitter_rust::LANGUAGE.into(),
        "fn main() { let x: Vec<i32> = vec![1, 2, 3]; for n in &x { println!(\"{}\", n); } }\n\
         struct Foo<T> { inner: T }\n\
         impl<T: Clone> Foo<T> { fn new(v: T) -> Self { Self { inner: v } } }\n",
    )];

    rows.push(audit(
        "python",
        tree_sitter_python::LANGUAGE.into(),
        "from typing import List\n\
         def square(xs: List[int]) -> List[int]:\n    return [x * x for x in xs]\n\
         class Foo:\n    def __init__(self, name: str): self.name = name\n    def greet(self): return f\"hi {self.name}\"\n",
    ));
    rows.push(audit(
        "javascript",
        tree_sitter_javascript::LANGUAGE.into(),
        "import { x } from './mod.js';\n\
         class Foo { constructor(n) { this.n = n; } greet() { return `hi ${this.n}`; } }\n\
         const sq = xs => xs.map(x => x * x);\nexport default Foo;\n",
    ));
    rows.push(audit(
        "typescript",
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "import { x } from './mod';\n\
         interface Greeter { greet(): string; }\n\
         class Foo implements Greeter { constructor(private n: string) {} greet(): string { return `hi ${this.n}`; } }\n\
         const sq = (xs: number[]): number[] => xs.map(x => x * x);\n",
    ));
    rows.push(audit(
        "tsx",
        tree_sitter_typescript::LANGUAGE_TSX.into(),
        "import React from 'react';\n\
         interface Props { name: string }\n\
         const Hello: React.FC<Props> = ({ name }) => <div className=\"hi\">{`hi ${name}`}</div>;\n\
         export default Hello;\n",
    ));
    rows.push(audit(
        "java",
        tree_sitter_java::LANGUAGE.into(),
        "package com.example;\n\
         import java.util.List;\n\
         public class Foo<T> { private final T v; public Foo(T v) { this.v = v; } public T get() { return v; } }\n\
         interface Greeter { String greet(); }\n",
    ));
    rows.push(audit(
        "kotlin",
        tree_sitter_kotlin::LANGUAGE.into(),
        "package com.example\n\
         class Foo(val n: String) { fun greet(): String = \"hi $n\" }\n\
         interface Greeter { fun greet(): String }\n\
         fun square(xs: List<Int>): List<Int> = xs.map { it * it }\n",
    ));
    rows.push(audit(
        "go",
        tree_sitter_go::LANGUAGE.into(),
        "package main\nimport \"fmt\"\n\
         type Foo struct { N string }\nfunc (f *Foo) Greet() string { return \"hi \" + f.N }\n\
         func main() { fmt.Println((&Foo{N: \"world\"}).Greet()) }\n",
    ));
    rows.push(audit(
        "c",
        tree_sitter_c::LANGUAGE.into(),
        "#include <stdio.h>\nstruct Foo { int n; };\n\
         int square(int x) { return x * x; }\n\
         int main(void) { struct Foo f = { .n = 3 }; printf(\"%d\\n\", square(f.n)); return 0; }\n",
    ));
    rows.push(audit(
        "cpp",
        tree_sitter_cpp::LANGUAGE.into(),
        "#include <vector>\n#include <string>\n\
         template <typename T> class Foo { T v; public: Foo(T x) : v(x) {} T get() const { return v; } };\n\
         int main() { Foo<std::string> f(\"hi\"); auto x = f.get(); return 0; }\n",
    ));
    rows.push(audit(
        "c_sharp",
        tree_sitter_c_sharp::LANGUAGE.into(),
        "namespace Example;\n\
         public interface IGreeter { string Greet(); }\n\
         public class Foo<T> : IGreeter { private readonly T _v; public Foo(T v) { _v = v; } public string Greet() => $\"hi {_v}\"; }\n",
    ));
    rows.push(audit(
        "php",
        tree_sitter_php::LANGUAGE_PHP.into(),
        "<?php\nnamespace App;\n\
         interface Greeter { public function greet(): string; }\n\
         class Foo implements Greeter { public function __construct(private string $n) {} public function greet(): string { return \"hi {$this->n}\"; } }\n",
    ));
    rows.push(audit(
        "ruby",
        tree_sitter_ruby::LANGUAGE.into(),
        "module App\n  class Foo < Base\n    include Greeter\n    def initialize(n); @n = n; end\n    def greet; \"hi #{@n}\"; end\n  end\nend\n",
    ));
    rows.push(audit(
        "swift",
        tree_sitter_swift::LANGUAGE.into(),
        "import Foundation\n\
         protocol Greeter { func greet() -> String }\n\
         class Foo: Greeter { let n: String; init(_ n: String) { self.n = n } func greet() -> String { return \"hi \\(n)\" } }\n",
    ));
    rows.push(audit(
        "dart",
        tree_sitter_dart::LANGUAGE.into(),
        "import 'dart:async';\n\
         abstract class Greeter { String greet(); }\n\
         class Foo implements Greeter { final String n; Foo(this.n); @override String greet() => 'hi $n'; }\n",
    ));
    rows.push(audit(
        "bash",
        tree_sitter_bash::LANGUAGE.into(),
        "#!/bin/bash\nset -euo pipefail\n\
         greet() { local name=\"$1\"; echo \"hi ${name}\"; }\n\
         for n in alice bob carol; do greet \"$n\"; done\n",
    ));
    rows.push(audit(
        "lua",
        tree_sitter_lua::LANGUAGE.into(),
        "local Foo = {}\nFoo.__index = Foo\n\
         function Foo.new(n) local self = setmetatable({}, Foo); self.n = n; return self end\n\
         function Foo:greet() return 'hi ' .. self.n end\nreturn Foo\n",
    ));
    rows.push(audit(
        "yaml",
        tree_sitter_yaml::LANGUAGE.into(),
        "name: example\nversion: 1.0\n\
         services:\n  web:\n    image: nginx:latest\n    ports:\n      - 80:80\n    environment:\n      - LOG_LEVEL=info\n",
    ));
    rows.push(audit(
        "markdown",
        tree_sitter_md::LANGUAGE.into(),
        "# Title\n\n## Subtitle\n\nThis is a paragraph with **bold** and `code`.\n\n- list item 1\n- list item 2\n\n```rust\nfn main() {}\n```\n",
    ));
    rows.push(audit(
        "solidity",
        tree_sitter_solidity::LANGUAGE.into(),
        "// SPDX-License-Identifier: MIT\npragma solidity ^0.8.0;\n\
         contract Foo { uint256 public n; constructor(uint256 _n) { n = _n; } function greet() public view returns (uint256) { return n; } }\n",
    ));
    rows.push(audit(
        "zig",
        tree_sitter_zig::LANGUAGE.into(),
        "const std = @import(\"std\");\n\
         pub fn main() !void { const stdout = std.io.getStdOut().writer(); try stdout.print(\"hi {}\\n\", .{42}); }\n\
         const Foo = struct { n: u32, fn greet(self: *const Foo) u32 { return self.n; } };\n",
    ));
    rows.push(audit(
        "hcl",
        tree_sitter_hcl::LANGUAGE.into(),
        "terraform { required_version = \">= 1.0\" }\n\
         resource \"aws_s3_bucket\" \"main\" { bucket = \"example-bucket\"; acl = \"private\"; tags = { Env = \"prod\" } }\n",
    ));
    rows.push(audit(
        "sql",
        tree_sitter_sequel::LANGUAGE.into(),
        "SELECT u.id, u.name, COUNT(o.id) AS cnt\n\
         FROM users u LEFT JOIN orders o ON o.user_id = u.id\n\
         WHERE u.created_at > NOW() - INTERVAL '30 days'\n\
         GROUP BY u.id, u.name HAVING COUNT(o.id) > 0;\n",
    ));
    rows.push(audit(
        "dockerfile",
        tree_sitter_containerfile::LANGUAGE.into(),
        "FROM alpine:3.18 AS base\nRUN apk add --no-cache curl\n\
         FROM base AS final\nWORKDIR /app\nCOPY . .\nEXPOSE 8080\nCMD [\"./run.sh\"]\n",
    ));
    rows.push(audit(
        "crystal",
        tree_sitter_crystal::LANGUAGE.into(),
        "module App\n  class Foo\n    def initialize(@n : String); end\n    def greet : String; \"hi #{@n}\"; end\n  end\nend\n",
    ));
    rows.push(audit(
        "cairo",
        tree_sitter_cairo::LANGUAGE.into(),
        "use core::array::ArrayTrait;\n\
         fn square(x: u32) -> u32 { x * x }\n\
         fn main() { let mut a = ArrayTrait::<u32>::new(); a.append(square(3)); }\n",
    ));
    rows.push(audit(
        "move",
        tree_sitter_move::LANGUAGE.into(),
        "module example::foo {\n  struct Foo has key { v: u64 }\n\
           public fun new(v: u64): Foo { Foo { v } }\n\
           public fun get(f: &Foo): u64 { f.v }\n}\n",
    ));
    rows.push(audit(
        "nim",
        tree_sitter_nim::language(),
        "import std/strformat\n\
         type Foo = object\n  n: string\n\
         proc greet(f: Foo): string = fmt\"hi {f.n}\"\n\
         echo greet(Foo(n: \"world\"))\n",
    ));
    rows.push(audit(
        "verilog",
        tree_sitter_verilog::LANGUAGE.into(),
        "module adder(input wire [7:0] a, b, output wire [8:0] sum);\n  assign sum = a + b;\nendmodule\n\
         module top; wire [8:0] s; adder u(8'h12, 8'h34, s); endmodule\n",
    ));
    rows.push(audit(
        "vyper",
        tree_sitter_vyper::LANGUAGE.into(),
        "n: public(uint256)\n\
         @external\ndef __init__(_n: uint256): self.n = _n\n\
         @view\n@external\ndef greet() -> uint256: return self.n\n",
    ));

    rows.sort_by(|a, b| a.abi.cmp(&b.abi).then(a.display.cmp(b.display)));

    println!();
    println!(
        "{:<12} {:>3} {:<22} {:>6} {:>5} {:>7} {:>10}",
        "display", "ABI", "name", "kinds", "flds", "src(B)", "parse(µs)"
    );
    println!("{}", "-".repeat(76));
    let mut abi14_total_us = 0.0;
    let mut abi14_count = 0;
    let mut abi15_total_us = 0.0;
    let mut abi15_count = 0;
    for r in &rows {
        println!(
            "{:<12} {:>3} {:<22} {:>6} {:>5} {:>7} {:>10.1}",
            r.display,
            r.abi,
            r.grammar_name,
            r.node_kinds,
            r.fields,
            r.snippet_bytes,
            r.per_parse_us
        );
        if r.abi >= 15 {
            abi15_total_us += r.per_parse_us;
            abi15_count += 1;
        } else {
            abi14_total_us += r.per_parse_us;
            abi14_count += 1;
        }
    }
    println!("{}", "-".repeat(76));
    if abi14_count > 0 {
        println!(
            "ABI <15 grammars: {:>2} mean={:>7.1}µs",
            abi14_count,
            abi14_total_us / abi14_count as f64
        );
    }
    if abi15_count > 0 {
        println!(
            "ABI ≥15 grammars: {:>2} mean={:>7.1}µs",
            abi15_count,
            abi15_total_us / abi15_count as f64
        );
    }
    println!();
}
