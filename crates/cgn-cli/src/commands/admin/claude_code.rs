//! `gnx admin install-hook --claude-code` / `uninstall-hook` / `status`.
//! Writes / removes per-event entries in Claude Code's settings.json,
//! preserving unrelated entries from other tools (e.g. legacy gitnexus
//! hook installs).

use clap::Args;
use cgn_core::GnxError;
use serde_json::{json, Map, Value};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Args, Debug, Clone)]
pub struct UninstallHookArgs {
    /// Target agent host. Exactly one host flag must be set.
    #[arg(long, default_value_t = false)]
    pub claude_code: bool,
    /// CSV of events to uninstall. Omit to remove all.
    #[arg(long)]
    pub events: Option<String>,
    /// Override path to settings.json (default `~/.claude/settings.json`).
    #[arg(long, hide = true)]
    pub settings_path: Option<PathBuf>,
}

#[derive(Args, Debug, Clone)]
pub struct StatusArgs {
    /// Target agent host. Exactly one host flag must be set.
    #[arg(long, default_value_t = false)]
    pub claude_code: bool,
    /// Override path to settings.json (default `~/.claude/settings.json`).
    #[arg(long, hide = true)]
    pub settings_path: Option<PathBuf>,
}

pub const ALL_EVENTS: &[&str] = &[
    "session-start",
    "user-prompt-submit",
    "pre-tool-use",
    "post-tool-use",
];

/// Entry point for `gnx admin install-hook --claude-code`.
/// Called from `install_hook::run` when the host flag is set.
pub fn run_install_claude_code(
    events_csv: Option<&str>,
    settings_path: Option<&Path>,
) -> Result<(), GnxError> {
    let events = match events_csv {
        Some(s) => parse_events(s)?,
        None => prompt_events_tui()?,
    };
    let settings_path = resolve_settings_path(settings_path);
    let mut settings = read_or_init(&settings_path)?;
    let exe = self_exe()?;
    for ev in &events {
        merge_entry(&mut settings, ev, &exe)?;
    }
    write_atomic(&settings_path, &settings)?;
    println!(
        "Installed {} event(s) into {}",
        events.len(),
        settings_path.display()
    );
    Ok(())
}

pub fn run_uninstall(args: UninstallHookArgs) -> Result<(), GnxError> {
    if !args.claude_code {
        return Err(GnxError::InvalidArgument("--claude-code required".into()));
    }
    let events = match &args.events {
        Some(s) => parse_events(s)?,
        None => ALL_EVENTS.iter().map(|s| (*s).to_string()).collect(),
    };
    let settings_path = resolve_settings_path(args.settings_path.as_deref());
    let mut settings = read_or_init(&settings_path)?;
    for ev in &events {
        remove_entry(&mut settings, ev);
    }
    write_atomic(&settings_path, &settings)?;
    println!(
        "Removed {} event(s) from {}",
        events.len(),
        settings_path.display()
    );
    Ok(())
}

pub fn run_status(args: StatusArgs) -> Result<(), GnxError> {
    if !args.claude_code {
        return Err(GnxError::InvalidArgument("--claude-code required".into()));
    }
    let settings_path = resolve_settings_path(args.settings_path.as_deref());
    let settings = read_or_init(&settings_path)?;
    println!(
        "Claude Code hook status (settings: {}):",
        settings_path.display()
    );
    for ev in ALL_EVENTS {
        let installed = is_installed(&settings, ev);
        let label = if installed { "INSTALLED" } else { "missing" };
        println!("  {:<22}  {}", ev, label);
    }
    Ok(())
}

// ─── internals ─────────────────────────────────────────────────────────────

fn parse_events(csv: &str) -> Result<Vec<String>, GnxError> {
    let mut out = Vec::new();
    for raw in csv.split(',') {
        let t = raw.trim();
        if t.is_empty() {
            continue;
        }
        if !ALL_EVENTS.contains(&t) {
            return Err(GnxError::InvalidArgument(format!(
                "unknown event '{t}' — expected one of: {}",
                ALL_EVENTS.join(", ")
            )));
        }
        out.push(t.to_string());
    }
    if out.is_empty() {
        return Err(GnxError::InvalidArgument("--events list is empty".into()));
    }
    Ok(out)
}

fn prompt_events_tui() -> Result<Vec<String>, GnxError> {
    use dialoguer::{theme::ColorfulTheme, MultiSelect};
    let chosen = MultiSelect::with_theme(&ColorfulTheme::default())
        .with_prompt("Select Claude Code hook events to install")
        .items(ALL_EVENTS)
        .interact()
        .map_err(|e| GnxError::Output(format!("TUI: {e}")))?;
    Ok(chosen
        .into_iter()
        .map(|i| ALL_EVENTS[i].to_string())
        .collect())
}

fn resolve_settings_path(override_path: Option<&Path>) -> PathBuf {
    if let Some(p) = override_path {
        return p.to_path_buf();
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"));
    home.join(".claude").join("settings.json")
}

fn read_or_init(path: &Path) -> Result<Value, GnxError> {
    if !path.exists() {
        return Ok(json!({"hooks": {}}));
    }
    let raw = fs::read_to_string(path)
        .map_err(|e| GnxError::Output(format!("read {}: {e}", path.display())))?;
    if raw.trim().is_empty() {
        return Ok(json!({"hooks": {}}));
    }
    serde_json::from_str(&raw)
        .map_err(|e| GnxError::InvalidArgument(format!("settings.json parse: {e}")))
}

fn self_exe() -> Result<String, GnxError> {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .map_err(|e| GnxError::Output(format!("current_exe: {e}")))
}

fn event_kebab_to_camel(ev: &str) -> &'static str {
    match ev {
        "session-start" => "SessionStart",
        "user-prompt-submit" => "UserPromptSubmit",
        "pre-tool-use" => "PreToolUse",
        "post-tool-use" => "PostToolUse",
        _ => unreachable!(),
    }
}

fn matcher_for(ev: &str) -> &'static str {
    match ev {
        "pre-tool-use" => "Grep|Glob|Bash",
        "post-tool-use" => "Bash",
        _ => "",
    }
}

fn timeout_for(ev: &str) -> u64 {
    match ev {
        "user-prompt-submit" => 3,
        "pre-tool-use" => 10,
        _ => 5,
    }
}

/// Extract the first hook command string from a settings.json hook
/// entry. Returns `""` when the entry is malformed in any of the five
/// expected layers (missing `hooks`, empty array, missing `command`,
/// non-string `command`). The empty default is safe because callers
/// only ever use `contains()` on the result.
fn command_of_entry(entry: &Value) -> &str {
    entry
        .get("hooks")
        .and_then(|h| h.as_array())
        .and_then(|hs| hs.first())
        .and_then(|h0| h0.get("command"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
}

fn merge_entry(settings: &mut Value, ev: &str, exe: &str) -> Result<(), GnxError> {
    let camel = event_kebab_to_camel(ev);
    let cmd = format!("\"{exe}\" hook {ev} --claude-code");

    let root = settings.as_object_mut().ok_or_else(|| {
        GnxError::InvalidArgument("settings.json root is not a JSON object".into())
    })?;
    let hooks_obj = root.entry("hooks").or_insert_with(|| json!({}));
    let hooks_map = hooks_obj.as_object_mut().ok_or_else(|| {
        GnxError::InvalidArgument("settings.json `hooks` field is not an object".into())
    })?;
    let arr_val = hooks_map
        .entry(camel.to_string())
        .or_insert_with(|| json!([]));
    let arr = arr_val.as_array_mut().ok_or_else(|| {
        GnxError::InvalidArgument(format!("settings.json `hooks.{camel}` is not an array"))
    })?;

    // Idempotence: drop any existing entry pointing at `gnx hook <ev>`.
    arr.retain(|e| {
        let c = command_of_entry(e);
        !(c.contains(&format!("hook {ev}")) && c.contains("--claude-code"))
    });

    let mut entry = Map::new();
    entry.insert("matcher".into(), Value::String(matcher_for(ev).into()));
    let mut h = Map::new();
    h.insert("type".into(), Value::String("command".into()));
    h.insert("command".into(), Value::String(cmd));
    h.insert("timeout".into(), Value::Number(timeout_for(ev).into()));
    if matches!(ev, "pre-tool-use") {
        h.insert(
            "statusMessage".into(),
            Value::String("Enriching with gnx graph context...".into()),
        );
    } else if matches!(ev, "post-tool-use") {
        h.insert(
            "statusMessage".into(),
            Value::String("Checking gnx index freshness...".into()),
        );
    }
    entry.insert("hooks".into(), Value::Array(vec![Value::Object(h)]));
    arr.push(Value::Object(entry));
    Ok(())
}

fn remove_entry(settings: &mut Value, ev: &str) {
    let camel = event_kebab_to_camel(ev);
    let Some(hooks_obj) = settings.get_mut("hooks").and_then(|h| h.as_object_mut()) else {
        return;
    };
    let Some(arr) = hooks_obj.get_mut(camel).and_then(|a| a.as_array_mut()) else {
        return;
    };
    arr.retain(|e| {
        let c = command_of_entry(e);
        !(c.contains(&format!("hook {ev}")) && c.contains("--claude-code"))
    });
}

fn is_installed(settings: &Value, ev: &str) -> bool {
    let camel = event_kebab_to_camel(ev);
    settings
        .get("hooks")
        .and_then(|h| h.get(camel))
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter().any(|e| {
                let c = command_of_entry(e);
                c.contains(&format!("hook {ev}")) && c.contains("--claude-code")
            })
        })
        .unwrap_or(false)
}

fn write_atomic(path: &Path, value: &Value) -> Result<(), GnxError> {
    cgn_core::registry::atomic_write_json(path, value)
        .map_err(|e| GnxError::Output(format!("atomic write {}: {e}", path.display())))
}
