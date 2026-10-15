//! A TS / JS import binds only a declaration its ES module exports, under
//! the name it is exported as (`export { g as f }` binds `f` to `g`,
//! FU-2026-10-15-1a48011441a4). The overlay reads a clean module's export
//! flags and aliases from `graph.bin` and a dirty module's from its fresh
//! parse, so it binds what a full reindex binds
//! (FU-2026-10-15-61d656576fc8).

#[path = "common/parity.rs"]
mod parity;

use ecp_analyzer::javascript::parser::JavaScriptProvider;
use ecp_analyzer::typescript::TypeScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use parity::{callees, calls_after_edit_in, named};

const MODULE: &str = "function f() {}\nfunction g() {}\nexport { g as f };\n";
const IMPORTER: &str = "import * as ns from './a';\nfunction use() {}\n";
const CALLING: &str = "import * as ns from './a';\nfunction use() { ns.f(); }\n";

fn providers() -> [(Box<dyn LanguageProvider>, &'static str); 2] {
    [
        (Box::new(TypeScriptProvider::new().unwrap()), "ts"),
        (Box::new(JavaScriptProvider::new().unwrap()), "js"),
    ]
}

/// The FU repro, edited into a dirty importer: both bind `g`, the
/// declaration `a` exports as `f`, never the private `f`.
#[test]
fn test_overlay_namespace_member_shadowed_by_export_alias_matches_reindex() {
    for (provider, ext) in providers() {
        let module = format!("a.{ext}");
        let importer = format!("b.{ext}");
        let parity = calls_after_edit_in(
            provider.as_ref(),
            &[(module.as_str(), MODULE), (importer.as_str(), IMPORTER)],
            &[(importer.as_str(), CALLING)],
            &named("use"),
        );
        assert_eq!(
            callees(&parity.reindex),
            [(module.as_str(), "g", 2, 0.95)],
            "{ext}: reindex"
        );
        assert_eq!(parity.overlay, parity.reindex, "{ext}: overlay vs reindex");
    }
}

/// The module is dirty too, and its edit drops the alias: the dirty
/// module's empty alias table overrides the archived one, so `f` is the
/// module's own exported `f` again.
#[test]
fn test_overlay_dirty_module_without_alias_matches_reindex() {
    for (provider, ext) in providers() {
        let module = format!("a.{ext}");
        let importer = format!("b.{ext}");
        let parity = calls_after_edit_in(
            provider.as_ref(),
            &[(module.as_str(), MODULE), (importer.as_str(), IMPORTER)],
            &[
                (importer.as_str(), CALLING),
                (module.as_str(), "export function f() {}\nfunction g() {}\n"),
            ],
            &named("use"),
        );
        assert_eq!(
            callees(&parity.reindex),
            [(module.as_str(), "f", 1, 0.95)],
            "{ext}: reindex"
        );
        assert_eq!(parity.overlay, parity.reindex, "{ext}: overlay vs reindex");
    }
}

/// The clean module exports nothing named `h`; its private `h` is the one
/// declaration of that name in the repo. The import names the module, so
/// the global tier must not bind the private `h` either.
#[test]
fn test_overlay_private_declaration_of_imported_module_left_unbound_like_reindex() {
    for (provider, ext) in providers() {
        let module = format!("a.{ext}");
        let importer = format!("b.{ext}");
        let parity = calls_after_edit_in(
            provider.as_ref(),
            &[
                (module.as_str(), "function h() {}\nexport function k() {}\n"),
                (
                    importer.as_str(),
                    "import { h } from './a';\nfunction use() {}\n",
                ),
            ],
            &[(
                &importer,
                "import { h } from './a';\nfunction use() { h(); }\n",
            )],
            &named("use"),
        );
        assert!(
            parity.reindex.is_empty(),
            "{ext}: reindex bound {:?}",
            parity.reindex
        );
        assert_eq!(parity.overlay, parity.reindex, "{ext}: overlay vs reindex");
    }
}
