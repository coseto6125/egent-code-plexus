//! Single source of truth for provider-`name` → constructor. `reanalyze::make_provider`
//! (incremental path) and `admin::index::detect_needed_providers` (cold-index path) both
//! resolve against this table instead of each re-listing its own `match` over provider
//! names — the two paths drifting out of sync silently dropped `.yaml` from being scanned
//! until PR #595. `ecp_core::analyzer::pipeline::AnalyzerPipeline::EXTENSION_TABLE` stays
//! the separate source of truth for extension → name; this table only maps name →
//! constructor and includes the two providers (markdown, yaml) that path resolution
//! never names directly, since they're reached only via the GitHub-Actions/docker-compose
//! path specials or explicit registration in `make_pipeline`.

use ecp_analyzer::{
    astro::parser::AstroProvider, bash::parser::BashProvider, c::parser::CProvider,
    c_sharp::parser::CSharpProvider, cairo::parser::CairoProvider, cpp::parser::CppProvider,
    crystal::parser::CrystalProvider, dart::parser::DartProvider,
    docker_compose::parser::DockerComposeProvider, dockerfile::parser::DockerfileProvider,
    github_actions::parser::GitHubActionsProvider, go::parser::GoProvider,
    hcl::parser::HclProvider, java::parser::JavaProvider, javascript::parser::JavaScriptProvider,
    kotlin::parser::KotlinProvider, kubernetes::parser::KubernetesProvider,
    lua::parser::LuaProvider, markdown::parser::MarkdownProvider, move_lang::parser::MoveProvider,
    nim::parser::NimProvider, openapi::schema_scan::OpenApiProvider, php::parser::PhpProvider,
    protobuf::parser::ProtobufProvider, python::parser::PythonProvider, ruby::parser::RubyProvider,
    rust::parser::RustProvider, solidity::parser::SolidityProvider, sql::parser::SqlProvider,
    svelte::parser::SvelteProvider, swift::parser::SwiftProvider,
    typescript::parser::TypeScriptProvider, verilog::parser::VerilogProvider,
    vue::parser::VueProvider, vyper::parser::VyperProvider, yaml::parser::YamlProvider,
    zig::parser::ZigProvider,
};
use ecp_core::analyzer::provider::LanguageProvider;

/// Builds one boxed `LanguageProvider`, or `Err` if construction fails (e.g.
/// a malformed tree-sitter query).
type ProviderCtor = fn() -> anyhow::Result<Box<dyn LanguageProvider>>;

/// `(provider name, constructor)`. Order is registration order for the full
/// pipeline (`reanalyze::make_pipeline`), which callers that assert exact
/// provider counts rely on staying stable.
pub const PROVIDER_CONSTRUCTORS: &[(&str, ProviderCtor)] = &[
    ("typescript", || Ok(Box::new(TypeScriptProvider::new()?))),
    ("python", || Ok(Box::new(PythonProvider::new()?))),
    ("go", || Ok(Box::new(GoProvider::new()?))),
    ("rust", || Ok(Box::new(RustProvider::new()?))),
    ("java", || Ok(Box::new(JavaProvider::new()?))),
    ("javascript", || Ok(Box::new(JavaScriptProvider::new()?))),
    ("php", || Ok(Box::new(PhpProvider::new()?))),
    ("ruby", || Ok(Box::new(RubyProvider::new()?))),
    ("kotlin", || Ok(Box::new(KotlinProvider::new()?))),
    ("c_sharp", || Ok(Box::new(CSharpProvider::new()?))),
    ("c", || Ok(Box::new(CProvider::new()?))),
    ("cpp", || Ok(Box::new(CppProvider::new()?))),
    ("swift", || Ok(Box::new(SwiftProvider::new()?))),
    ("dart", || Ok(Box::new(DartProvider::new()?))),
    ("markdown", || {
        MarkdownProvider::new()
            .map(|p| Box::new(p) as Box<dyn LanguageProvider>)
            .map_err(|e| anyhow::anyhow!(e))
    }),
    ("yaml", || Ok(Box::new(YamlProvider::new()?))),
    ("github-actions", || {
        Ok(Box::new(GitHubActionsProvider::new()?))
    }),
    ("bash", || Ok(Box::new(BashProvider::new()?))),
    ("lua", || Ok(Box::new(LuaProvider::new()?))),
    ("crystal", || Ok(Box::new(CrystalProvider::new()?))),
    ("move", || Ok(Box::new(MoveProvider::new()?))),
    ("solidity", || Ok(Box::new(SolidityProvider::new()?))),
    ("dockerfile", || Ok(Box::new(DockerfileProvider::new()?))),
    ("nim", || Ok(Box::new(NimProvider::new()?))),
    ("hcl", || Ok(Box::new(HclProvider::new()?))),
    ("sql", || Ok(Box::new(SqlProvider::new()?))),
    ("vyper", || Ok(Box::new(VyperProvider::new()?))),
    ("verilog", || Ok(Box::new(VerilogProvider::new()?))),
    ("cairo", || Ok(Box::new(CairoProvider::new()?))),
    ("zig", || Ok(Box::new(ZigProvider::new()?))),
    ("vue", || Ok(Box::new(VueProvider::new()?))),
    ("astro", || Ok(Box::new(AstroProvider::new()?))),
    ("svelte", || Ok(Box::new(SvelteProvider::new()?))),
    ("docker-compose", || {
        Ok(Box::new(DockerComposeProvider::new()?))
    }),
    ("protobuf", || Ok(Box::new(ProtobufProvider::new()?))),
    ("openapi", || Ok(Box::new(OpenApiProvider::new()?))),
    ("kubernetes", || Ok(Box::new(KubernetesProvider::new()?))),
];

/// Construct one provider by its `provider_name_for_path`-compatible name.
/// `None` for an unknown name.
pub fn make_provider(name: &str) -> Option<anyhow::Result<Box<dyn LanguageProvider>>> {
    PROVIDER_CONSTRUCTORS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, ctor)| ctor())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecp_core::analyzer::pipeline::AnalyzerPipeline;
    use rustc_hash::FxHashSet;

    /// Every name `provider_name_for_path` can produce must resolve to a
    /// constructible provider — the extension table and the constructor
    /// table must not drift out of sync (this repo has shipped two silent
    /// dispatch-table drift bugs: PR #141/#142, PR #595).
    #[test]
    fn provider_constructors_test_every_extension_table_name_is_constructible() {
        let mut names: FxHashSet<&str> = AnalyzerPipeline::EXTENSION_TABLE
            .iter()
            .map(|(_, name)| *name)
            .collect();
        // Path-based special-case names not present in EXTENSION_TABLE.
        names.insert("docker-compose");
        names.insert("github-actions");
        names.insert("kubernetes");

        for name in names {
            assert!(
                make_provider(name).is_some(),
                "PROVIDER_CONSTRUCTORS has no entry for {name:?}, which provider_name_for_path can return"
            );
        }
    }

    #[test]
    fn provider_constructors_test_registry_names_unique() {
        let mut seen: FxHashSet<&str> = FxHashSet::default();
        for (name, _) in PROVIDER_CONSTRUCTORS {
            assert!(
                seen.insert(name),
                "duplicate provider name in registry: {name:?}"
            );
        }
    }

    #[test]
    fn provider_constructors_test_every_entry_constructs_successfully() {
        for (name, ctor) in PROVIDER_CONSTRUCTORS {
            assert!(ctor().is_ok(), "constructor for {name:?} returned Err");
        }
    }
}
