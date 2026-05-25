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

    // Gatekeeper: any top-level command with `--repo @<group>` exits early
    // with a migration hint pointing at `ecp group …`. Runs before all
    // dispatch so the message is identical regardless of graph-free vs
    // graph-loading path. `@all` is unaffected (still resolves to the
    // registered repo set).
    check_group_atom(&cli);

    // Admin: subcommand → run the admin operation; no subcommand → launch TUI.
    if let Commands::Admin { command } = cli.command {
        let err = match command {
            Some(cmd) => commands::admin::run(cmd, Cli::command()),
            None => admin::run(admin::AdminArgs {}),
        };
        if let Err(e) = err {
            eprintln!("Command failed: {e}");
            std::process::exit(1);
        }
        return;
    }

    // Dispatch table for commands that don't need a graph loaded.
    macro_rules! run_no_graph {
        ($expr:expr) => {{
            if let Err(e) = $expr {
                eprintln!("Command failed: {e}");
                std::process::exit(1);
            }
            return;
        }};
    }

    match &cli.command {
        Commands::HookHandle(args) => run_no_graph!(commands::hook_handle::run(args.clone())),
        Commands::HookWatcher(args) => run_no_graph!(commands::hook_watcher::run(args.clone())),
        Commands::Summary(args) => {
            run_no_graph!(commands::summary::run(args.clone(), &cli.graph))
        }
        Commands::Dev { command } => run_no_graph!(commands::dev::run(command.clone(), &cli.graph)),
        Commands::Contracts(args) => run_no_graph!(commands::contracts::run(args.clone())),
        Commands::Diff(args) => run_no_graph!(commands::diff::run(args.clone())),
        Commands::Hook(args) => run_no_graph!(commands::hook::run(args.clone())),
        Commands::Watch(args) => run_no_graph!(commands::watch::run(args.clone())),
        Commands::Peers(args) => run_no_graph!(commands::peers::run(args.clone())),
        Commands::Group { cmd } => run_no_graph!(commands::group::run(cmd.clone())),
        Commands::Schema(args) => run_no_graph!(commands::schema::run(args.clone())),
        Commands::Insight(args) => run_no_graph!(commands::insight::run(args.clone())),
        Commands::Uninstall(args) => {
            run_no_graph!(commands::uninstall::run(args.clone()))
        }
        _ => {} // fall through to graph-loading path
    }

    // Agent commands + ShapeCheck (hidden internal) — need graph
    let repo_opt = match &cli.command {
        Commands::Inspect(args) => args.repo.as_deref(),
        Commands::Find(args) => args.repo.as_deref(),
        Commands::Impact(args) => args.repo.as_deref(),
        Commands::Rename(args) => args.repo.as_deref(),
        Commands::Cypher(args) => args.repo.as_deref(),
        Commands::Routes(args) => args.repo.as_deref(),
        Commands::ShapeCheck(args) => args.repo.as_deref(),
        Commands::ToolMap(args) => args.repo.as_deref(),
        Commands::Review(args) => args.repo.as_deref(),
        Commands::FindTransactionPatterns(args) => args.repo.as_deref(),
        Commands::Processes(args) => args.repo.as_deref(),
        Commands::FindSchemaBindings(args) => args.repo.as_deref(),
        Commands::FindEventMirrors(args) => args.repo.as_deref(),
        Commands::Summary(_)
        | Commands::Contracts(_)
        | Commands::Diff(_)
        | Commands::Admin { .. }
        | Commands::Dev { .. }
        | Commands::HookHandle(_)
        | Commands::HookWatcher(_)
        | Commands::Hook(_)
        | Commands::Watch(_)
        | Commands::Peers(_)
        | Commands::Group { .. }
        | Commands::Schema(_)
        | Commands::Insight(_)
        | Commands::Uninstall(_) => None,
    };
    let cwd = repo_opt
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let mut graph_path = graph_path::resolve(&cli.graph, &cwd);

    // An explicit `--graph <path>` is taken literally. If it does not exist,
    // error rather than warm-attaching to cwd's graph — answering a directed
    // query against the wrong graph is worse than an honest failure.
    if graph_path::is_custom(&cli.graph) && !graph_path.exists() {
        eprintln!(
            "Error: --graph path does not exist: {}",
            graph_path.display()
        );
        std::process::exit(1);
    }

    let engine = match auto_ensure::ensure_fresh(&graph_path, &cwd) {
        Err(err) => {
            eprintln!("Error preparing index for {}: {err}", cwd.display());
            std::process::exit(1);
        }
        Ok(auto_ensure::EnsureFreshOutcome::WarmAttach { sibling_graph_path }) => {
            // New HEAD has no published graph. Load the sibling SHA's graph
            // immediately so this invocation is not blocked; the real rebuild
            // is running in the background.
            eprintln!("note: results may be slightly stale (warm-attach, rebuild in progress)");
            match Engine::load_warm(&sibling_graph_path) {
                Ok(e) => e,
                Err(err) => {
                    eprintln!(
                        "Error loading warm-attach graph from {}: {}",
                        sibling_graph_path.display(),
                        err
                    );
                    std::process::exit(1);
                }
            }
        }
        Ok(auto_ensure::EnsureFreshOutcome::Ready) => {
            graph_path = graph_path::resolve(&cli.graph, &cwd);
            match Engine::load(&graph_path) {
                Ok(e) => e,
                Err(err) => {
                    eprintln!("Error loading graph from {}: {}", graph_path.display(), err);
                    std::process::exit(1);
                }
            }
        }
    };

    let result: Result<(), ecp_core::EcpError> = match cli.command {
        Commands::Inspect(args) => commands::inspect::run(args, &engine, &graph_path),
        Commands::Find(args) => commands::find::run(args, &engine),
        Commands::Impact(args) => commands::impact::run(args, &engine),
        Commands::Rename(args) => commands::rename::run(args, &engine),
        Commands::Cypher(args) => commands::cypher::run(args, &engine),
        Commands::Routes(args) => commands::routes::run(args, &engine),
        Commands::ShapeCheck(args) => commands::shape_check::run(args, &engine),
        Commands::ToolMap(args) => commands::tool_map::run(args, &engine),
        Commands::Review(args) => commands::review::run(args, &engine),
        Commands::FindTransactionPatterns(args) => commands::find_tx_patterns::run(args, &engine),
        Commands::FindSchemaBindings(args) => commands::find_schema_bindings::run(args, &engine),
        Commands::FindEventMirrors(args) => commands::find_event_mirrors::run(args, &engine),
        Commands::Processes(args) => commands::processes::run(args, &engine),
        Commands::Summary(_)
        | Commands::Contracts(_)
        | Commands::Diff(_)
        | Commands::Admin { .. }
        | Commands::Dev { .. }
        | Commands::HookHandle(_)
        | Commands::HookWatcher(_)
        | Commands::Hook(_)
        | Commands::Watch(_)
        | Commands::Peers(_)
        | Commands::Group { .. }
        | Commands::Schema(_)
        | Commands::Insight(_)
        | Commands::Uninstall(_) => unreachable!("handled before graph load"),
    };
    if let Err(e) = result {
        eprintln!("Command failed: {e}");
        std::process::exit(1);
    }
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
        Commands::Cypher(a) => (a.repo.as_deref(), None),
        Commands::Routes(a) => (a.repo.as_deref(), None),
        Commands::ShapeCheck(a) => (a.repo.as_deref(), None),
        Commands::ToolMap(a) => (a.repo.as_deref(), None),
        Commands::Review(a) => (a.repo.as_deref(), None),
        Commands::Diff(a) => (a.repo.as_deref(), None),
        Commands::FindTransactionPatterns(a) => (a.repo.as_deref(), None),
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
