//! Macro definitions for tool self-registration.

/// Coerce a `Cow<'static, str>` (as returned by schemars 1.x
/// `JsonSchema::schema_name()`) into a `&'static str`.
///
/// For the `Borrowed` variant, which is what derive-generated impls always
/// produce, this is a zero-cost cast. For the `Owned` variant (hand-written
/// impls that return a formatted string) we leak once — bounded by the
/// finite set of registered tools (≤30) so the total leaked bytes are
/// negligible.
pub fn cow_to_static(cow: std::borrow::Cow<'static, str>) -> &'static str {
    match cow {
        std::borrow::Cow::Borrowed(s) => s,
        std::borrow::Cow::Owned(s) => Box::leak(s.into_boxed_str()),
    }
}

/// Register a CLI command as an MCP tool. Called once at the bottom of
/// each `commands/<x>.rs` file that should appear as an MCP tool.
///
/// Tool name is auto-derived from `module_path!()` — adding the file
/// to the module tree is enough.
///
/// # Arguments
/// - `$args:ty` — the command's Args struct type (must derive
///   `Serialize + Deserialize + JsonSchema`)
/// - `$inner:path` — fully-qualified path to the `run_inner` function
///   that takes `$args` and an `&dyn EngineRef` (or compatible) and
///   returns `Result<serde_json::Value, GnxError>`.
///
/// # inventory::submit! const requirement
/// `inventory::submit!` requires all struct-field initializers to be
/// const-evaluable (the value lives in a static). Non-const computations
/// (`derive_tool_name`, `schema_name()`) are therefore wrapped in zero-arg
/// function pointers (`fn() -> &'static str` / `fn() -> Schema`), which
/// are themselves const. The GnxMcpTool fields `name`, `description`, and
/// `subcommand` are function pointers for this reason.
///
/// # schemars 1.x note
/// `JsonSchema::schema_name()` returns `Cow<'static, str>` in schemars 1.x.
/// `crate::macros::cow_to_static` handles both variants: `Borrowed` is
/// zero-cost; `Owned` is leaked once per registered tool.
#[macro_export]
macro_rules! gnx_register_mcp_tool {
    ($args:ty, $inner:path) => {
        inventory::submit! {
            $crate::registry::GnxMcpTool {
                name: || {
                    static N: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
                    *N.get_or_init(|| $crate::registry::derive_tool_name(module_path!()))
                },
                description: || {
                    static D: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
                    *D.get_or_init(|| $crate::macros::cow_to_static(
                        <$args as ::schemars::JsonSchema>::schema_name()
                    ))
                },
                schema: || ::schemars::schema_for!($args),
                handler: |raw, engine| {
                    let parsed: $args = ::serde_json::from_value(raw)
                        .map_err(|e| ::graph_nexus_core::GnxError::InvalidArgument(
                            format!("MCP args decode: {e}")))?;
                    $inner(parsed, engine)
                },
                subcommand: || $crate::registry::derive_subcommand(module_path!()),
            }
        }
    };
}
