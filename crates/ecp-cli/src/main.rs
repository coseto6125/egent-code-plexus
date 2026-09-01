use clap::{CommandFactory, Parser};
use ecp_cli::cli::{Cli, Commands};
use ecp_cli::commands;
use ecp_cli::engine::Engine;
use ecp_cli::{admin, auto_ensure, graph_path};

// mimalloc as global allocator: heavily parallel build path
// (16-thread rayon par_iter on 22k file parses + cache puts + edge
// emission) hammers the system allocator. mimalloc's per-thread
// arenas dramatically reduce allocator lock contention vs glibc
// malloc, especially for the many short-lived Vec/String allocations
// in tree-sitter capture processing + post-process edge resolution.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() {
    // Default to WARN so tantivy / parser INFO chatter doesn't pollute
    // agents' output. Writer is stderr so `ecp ... --format json | jq` and
    // test harnesses parsing stdout stay clean; logs still surface for
    // human debugging via 2>&1 or capturing stderr.
    // `tracing_subscriber::fmt()` defaults to stdout — must opt into
    // stderr explicitly. RUST_LOG=info|debug overrides the level.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    maybe_spawn_background_gc();

    let cli = Cli::parse();
    // `command_label` + `subcommand_label` borrow `&cli.command` before
    // `dispatch(cli)` consumes `cli`; compute both first so dispatch can take
    // ownership. `subcommand` stays None for flat verbs (find, impact, …);
    // populated for `admin <sub>`, `dev <sub>`, `group <sub>` so `ecp usage`
    // can drill into `admin gc` vs `admin index` vs the bare TUI.
    let label = command_label(&cli.command);
    let sub = subcommand_label(&cli.command);
    let start = std::time::Instant::now();
    let outcome = dispatch(cli);
    let duration_ms = start.elapsed().as_millis() as u64;
    ecp_cli::telemetry_cli::record(label, sub, duration_ms, outcome.as_ref().err());
    // The answer is already on stdout; a cold-index build may still be
    // writing its tantivy index, and exiting now would publish an empty one.
    ecp_cli::auto_ensure::join_pending_tantivy_writers();
    if let Err(e) = outcome {
        eprintln!("Command failed: {e}");
        std::process::exit(1);
    }
}

/// Stable nested-verb label for telemetry. `None` for flat commands;
/// `Some("gc")` / `Some("index")` / `Some("group sync")` for nested ones.
/// Not exhaustive — unknown variants safely degrade to `None` so a new
/// admin/dev/group subcommand still indexes under the parent verb.
fn subcommand_label(cmd: &Commands) -> Option<&'static str> {
    use ecp_cli::commands;
    match cmd {
        Commands::Admin { command: Some(c) } => Some(match c {
            commands::admin::AdminCommands::InstallHook(_) => "install-hook",
            commands::admin::AdminCommands::UninstallHook(_) => "uninstall-hook",
            commands::admin::AdminCommands::Status(_) => "status",
            commands::admin::AdminCommands::Drop(_) => "drop",
            commands::admin::AdminCommands::Prune(_) => "prune",
            commands::admin::AdminCommands::Gc(_) => "gc",
            commands::admin::AdminCommands::Config(_) => "config",
            commands::admin::AdminCommands::Group { .. } => "group",
            commands::admin::AdminCommands::Index(_) => "index",
            commands::admin::AdminCommands::Sessions { .. } => "sessions",
            commands::admin::AdminCommands::Claude { .. } => "claude",
            commands::admin::AdminCommands::Codex { .. } => "codex",
            commands::admin::AdminCommands::Gemini { .. } => "gemini",
            commands::admin::AdminCommands::Mcp(_) => "mcp",
            commands::admin::AdminCommands::Doctor(_) => "doctor",
            commands::admin::AdminCommands::CheckUpdate => "check-update",
        }),
        Commands::Admin { command: None } => Some("tui"),
        Commands::Dev { command } => Some(match command {
            commands::dev::DevCommands::PrAnalyze(_) => "pr-analyze",
            commands::dev::DevCommands::UidAudit(_) => "uid-audit",
            commands::dev::DevCommands::VerifyResolver(_) => "verify-resolver",
        }),
        Commands::Group { cmd } => Some(match cmd {
            commands::group::GroupCommands::Sync(_) => "sync",
            commands::group::GroupCommands::Status(_) => "status",
            commands::group::GroupCommands::Contracts(_) => "contracts",
            commands::group::GroupCommands::Impact(_) => "impact",
            commands::group::GroupCommands::Find(_) => "find",
            commands::group::GroupCommands::Summary(_) => "summary",
        }),
        _ => None,
    }
}

/// Stable `ecp <verb>` label for every `Commands` variant. Used as the
/// telemetry `tool` field; must stay exhaustive so a new variant forces a
/// label decision at compile time.
fn command_label(cmd: &Commands) -> &'static str {
    match cmd {
        Commands::Inspect(_) => "inspect",
        Commands::Find(_) => "find",
        Commands::Impact(_) => "impact",
        Commands::Rename(_) => "rename",
        Commands::Pattern(_) => "pattern",
        Commands::Cypher(_) => "cypher",
        Commands::Routes(_) => "routes",
        Commands::ShapeCheck(_) => "shape-check",
        Commands::ToolMap(_) => "tool-map",
        Commands::Review(_) => "review",
        Commands::Heuristics(_) => "heuristics",
        Commands::FindTransactionPatterns(_) => "find-transaction-patterns",
        Commands::FindSchemaBindings(_) => "find-schema-bindings",
        Commands::FindEventMirrors(_) => "find-event-mirrors",
        Commands::Path(_) => "path",
        Commands::Processes(_) => "processes",
        Commands::Summary(_) => "summary",
        Commands::Contracts(_) => "contracts",
        Commands::Diff(_) => "diff",
        Commands::Admin { .. } => "admin",
        Commands::Dev { .. } => "dev",
        Commands::HookHandle(_) => "hook-handle",
        Commands::HookWatcher(_) => "hook-watcher",
        Commands::Hook(_) => "hook",
        Commands::Watch(_) => "watch",
        Commands::Peers(_) => "peers",
        Commands::Group { .. } => "group",
        Commands::Schema(_) => "schema",
        Commands::Insight(_) => "insight",
        Commands::Usage(_) => "usage",
        Commands::Uninstall(_) => "uninstall",
    }
}

/// Single recorded dispatch path: every command (graph-free or graph-loading)
/// returns through here so `main` can time + record telemetry once and own the
/// sole `exit(1)`. Graph-load failures that used to `eprintln!+exit` inline are
/// now `Err(EcpError::InvalidArgument(..))` carrying the same message text.
fn dispatch(cli: Cli) -> Result<(), ecp_core::EcpError> {
    // Gatekeeper: any top-level command with `--repo @<group>` exits early
    // with a migration hint pointing at `ecp group …`. Runs before all
    // dispatch so the message is identical regardless of graph-free vs
    // graph-loading path. `@all` is unaffected (still resolves to the
    // registered repo set).
    check_group_atom(&cli);

    // Admin: subcommand → run the admin operation; no subcommand → launch TUI.
    if let Commands::Admin { command } = cli.command {
        return match command {
            Some(cmd) => commands::admin::run(cmd, Cli::command()),
            None => admin::run(admin::AdminArgs {}),
        };
    }

    // graph_path::resolve / auto_ensure do real I/O (git HEAD lookup, commit
    // index scan, background reindex spawn); graph-free commands must not pay
    // for it, so the whole load is skipped rather than run and discarded —
    // down to the `current_dir()` syscall.
    let (engine, graph_path) = if cli.command.needs_graph() {
        let cwd = cli
            .command
            .repo()
            .map(std::path::PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let mut graph_path = graph_path::resolve(&cli.graph, &cwd);

        // An explicit `--graph <path>` is taken literally. If it does not exist,
        // error rather than warm-attaching to cwd's graph — answering a directed
        // query against the wrong graph is worse than an honest failure.
        if graph_path::is_custom(&cli.graph) && !graph_path.exists() {
            return Err(ecp_core::EcpError::InvalidArgument(format!(
                "Error: --graph path does not exist: {}",
                graph_path.display()
            )));
        }

        let engine =
            match auto_ensure::ensure_fresh(auto_ensure::IndexNeed::NearCurrent, &graph_path, &cwd)
            {
                Err(err) => {
                    return Err(ecp_core::EcpError::InvalidArgument(format!(
                        "Error preparing index for {}: {err}",
                        cwd.display()
                    )));
                }
                Ok(auto_ensure::EnsureFreshOutcome::WarmAttach { sibling_graph_path }) => {
                    // The sibling passed auto_ensure's distance gate (≤1 commit behind),
                    // so it serves silently while the background rebuild runs. No "may be
                    // stale" note: a per-invocation apology for a near-current graph is
                    // context noise the LLM can't act on, and a too-stale sibling never
                    // reaches this arm. The actionable hint is reserved for load failure.
                    match Engine::load_warm(&sibling_graph_path) {
                        Ok(e) => e,
                        Err(err) => {
                            return Err(ecp_core::EcpError::InvalidArgument(format!(
                                "warm-attach graph load failed ({}): {err}. \
                             Rebuild the index with `ecp admin index --force --repo .`",
                                sibling_graph_path.display(),
                            )));
                        }
                    }
                }
                Ok(auto_ensure::EnsureFreshOutcome::Ready) => {
                    graph_path = graph_path::resolve(&cli.graph, &cwd);
                    match Engine::load(&graph_path) {
                        Ok(e) => e,
                        Err(err) => {
                            return Err(ecp_core::EcpError::InvalidArgument(format!(
                                "Error loading graph from {}: {}",
                                graph_path.display(),
                                err
                            )));
                        }
                    }
                }
            };

        // Attach the session's L1 overlay dir (if one resolves) so query commands
        // can surface uncommitted working-tree edits the L2 graph hasn't absorbed.
        // Agent hosts inject a stable session id (ECP_SESSION_ID /
        // CLAUDE_CODE_SESSION_ID); a bare CLI invocation falls back to a
        // per-process id, so the fragments `ensure_fresh` just wrote above are
        // still read back by this same process. Empty/absent overlay = zero
        // query-path cost.
        let engine = match auto_ensure::resolve_session_overlay_dir(&cwd) {
            Some(dir) if dir.is_dir() => engine.with_overlay(dir, cwd.clone()),
            _ => engine,
        };

        (Some(engine), Some(graph_path))
    } else {
        (None, None)
    };
    let engine = engine.as_ref();

    let result: Result<(), ecp_core::EcpError> = match cli.command {
        Commands::Inspect(args) => commands::inspect::run(
            args,
            engine.expect("needs_graph() gates this arm"),
            graph_path.as_ref().expect("needs_graph() gates this arm"),
        ),
        Commands::Find(args) => {
            commands::find::run(args, engine.expect("needs_graph() gates this arm"))
        }
        Commands::Impact(args) => {
            commands::impact::run(args, engine.expect("needs_graph() gates this arm"))
        }
        Commands::Rename(args) => {
            commands::rename::run(args, engine.expect("needs_graph() gates this arm"))
        }
        Commands::Pattern(args) => {
            commands::pattern::run(args, engine.expect("needs_graph() gates this arm"))
        }
        Commands::Cypher(args) => {
            commands::cypher::run(args, engine.expect("needs_graph() gates this arm"))
        }
        Commands::Routes(args) => {
            commands::routes::run(args, engine.expect("needs_graph() gates this arm"))
        }
        Commands::ShapeCheck(args) => {
            commands::shape_check::run(args, engine.expect("needs_graph() gates this arm"))
        }
        Commands::ToolMap(args) => {
            commands::tool_map::run(args, engine.expect("needs_graph() gates this arm"))
        }
        Commands::Review(args) => {
            commands::review::run(args, engine.expect("needs_graph() gates this arm"))
        }
        Commands::Heuristics(args) => {
            commands::heuristics::run(args, engine.expect("needs_graph() gates this arm"))
        }
        Commands::FindTransactionPatterns(args) => {
            commands::find_tx_patterns::run(args, engine.expect("needs_graph() gates this arm"))
        }
        Commands::FindSchemaBindings(args) => {
            commands::find_schema_bindings::run(args, engine.expect("needs_graph() gates this arm"))
        }
        Commands::FindEventMirrors(args) => {
            commands::find_event_mirrors::run(args, engine.expect("needs_graph() gates this arm"))
        }
        Commands::Path(args) => {
            commands::path::run(args, engine.expect("needs_graph() gates this arm"))
        }
        Commands::Processes(args) => {
            commands::processes::run(args, engine.expect("needs_graph() gates this arm"))
        }
        Commands::HookHandle(args) => commands::hook_handle::run(args),
        Commands::HookWatcher(args) => commands::hook_watcher::run(args),
        Commands::Summary(args) => commands::summary::run(args, &cli.graph),
        Commands::Dev { command } => commands::dev::run(command, &cli.graph),
        Commands::Contracts(args) => commands::contracts::run(args),
        Commands::Diff(args) => commands::diff::run(args),
        Commands::Hook(args) => commands::hook::run(args),
        Commands::Watch(args) => commands::watch::run(args),
        Commands::Peers(args) => commands::peers::run(args).map_err(ecp_core::EcpError::from),
        Commands::Group { cmd } => commands::group::run(cmd),
        Commands::Schema(args) => commands::schema::run(args),
        Commands::Insight(args) => commands::insight::run(args),
        Commands::Usage(args) => commands::usage::run(args),
        Commands::Uninstall(args) => commands::uninstall::run(args),
        Commands::Admin { .. } => unreachable!("handled before graph load"),
    };
    result
}

/// Top-level `--repo @<group>` rejection. The atom is meaningful only inside
/// `ecp group <verb>`; on every other command it is either a path-not-found
/// (auto_ensure) or a single-repo selector that silently expands and fails
/// later with an opaque message. Catch it here and exit with a clear hint.
///
/// The `hint` is the `ecp group <verb>` migration target. Commands without a
/// group analog (inspect / rename / cypher / routes / shape-check / tool-map
/// / review / diff) carry `None` and get redirected to `ecp group --help`.
fn check_group_atom(cli: &Cli) {
    // The `repo: Option<String>` accessor lives on each variant's args struct,
    // so the match has to enumerate them. Pull the value out first; bail
    // fast for commands without a `--repo` field (contracts/summary are
    // already protected via resolve_top_level; peers/admin/hooks don't expose
    // a group-aware selector).
    let (repo_opt, hint): (Option<&str>, Option<&str>) = match &cli.command {
        Commands::Find(a) => (a.repo.as_deref(), Some("find")),
        Commands::Impact(a) => (a.repo.as_deref(), Some("impact")),
        Commands::Inspect(a) => (a.repo.as_deref(), None),
        Commands::Rename(a) => (a.repo.as_deref(), None),
        Commands::Pattern(a) => (a.repo.as_deref(), None),
        Commands::Cypher(a) => (a.repo.as_deref(), None),
        Commands::Routes(a) => (a.repo.as_deref(), None),
        Commands::ShapeCheck(a) => (a.repo.as_deref(), None),
        Commands::ToolMap(a) => (a.repo.as_deref(), None),
        Commands::Review(a) => (a.repo.as_deref(), None),
        Commands::Diff(a) => (a.repo.as_deref(), None),
        Commands::Heuristics(a) => {
            let repo = match &a.kind {
                commands::heuristics::HeuristicsKind::Saga(x) => x.repo.as_deref(),
                commands::heuristics::HeuristicsKind::SchemaBindings(x) => x.repo.as_deref(),
                commands::heuristics::HeuristicsKind::EventMirrors(x) => x.repo.as_deref(),
            };
            (repo, None)
        }
        Commands::FindTransactionPatterns(a) => (a.repo.as_deref(), None),
        Commands::Path(a) => (a.repo.as_deref(), None),
        Commands::Processes(a) => (a.repo.as_deref(), None),
        Commands::FindSchemaBindings(a) => (a.repo.as_deref(), None),
        Commands::FindEventMirrors(a) => (a.repo.as_deref(), None),
        _ => return,
    };
    // The vast majority of invocations don't pass `--repo` at all, so the
    // two early returns below fire before any further work.
    let Some(sel) = repo_opt else { return };
    let Some(group_name) = sel.strip_prefix('@') else {
        return;
    };
    if group_name == "all" {
        return;
    }
    match hint {
        Some(verb) => eprintln!(
            "error: `@{group_name}` cannot be used at the top level — use `ecp group {verb}` instead"
        ),
        None => eprintln!(
            "error: `@{group_name}` cannot be used at the top level — this command is single-repo; see `ecp group --help` for cross-repo workflows"
        ),
    }
    std::process::exit(1);
}

/// Auto-trigger background GC when the home heartbeat stamp is missing
/// or older than 24h. Spawned detached; failures are silent (best-effort).
fn maybe_spawn_background_gc() {
    let home = ecp_core::registry::resolve_home_ecp();
    let stamp = home.join(".last-gc");
    let stale = std::fs::metadata(&stamp)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| std::time::SystemTime::now().duration_since(t).ok())
        .map(|d| d.as_secs() > 24 * 3600)
        .unwrap_or(true);
    if !stale {
        return;
    }
    // Touch the stamp synchronously so concurrent CLI invocations don't all spawn.
    let _ = std::fs::create_dir_all(&home);
    let _ = std::fs::write(&stamp, b"");
    // Detach background sweep — gc admin command not yet wired (Phase 8 Task 8.5),
    // so until then this fn just touches the stamp. Once `ecp admin gc` lands,
    // change the body to spawn it as a detached subprocess.
}
