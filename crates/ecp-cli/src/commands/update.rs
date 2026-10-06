//! `ecp update` — replace the running binary with the latest GitHub Release.
//!
//! Download and extraction go through `curl` and `tar`, which every supported
//! platform ships (Windows 10 1803+ carries both, and its bsdtar reads zip), so
//! the binary gains no HTTP or archive dependency. The release's `.sha256`
//! sidecar is verified before anything is swapped.
//!
//! The staged binary is run with `--version` before anything is swapped, so a
//! release that cannot start on this host never replaces one that can. On
//! Unix the swap is one atomic rename over the running file, whose inode
//! stays alive for this process. Windows refuses that, but lets a running
//! executable be renamed: it moves aside to `<exe>.old`, the new file moves
//! in, and the `.old` file (still mapped, so not unlinkable) is swept on the
//! next update.
//!
//! The `.sha256` sidecar comes from the same origin as the archive, so it
//! catches a truncated or corrupted download, not a compromised release. The
//! SLSA attestation the release publishes is checked through `gh attestation
//! verify` when `gh` is installed and signed in; otherwise the update says
//! that provenance went unchecked and continues.
//!
//! Channel installs (npm / uv / pip / brew / cargo) are replaced the same way;
//! the package manager's own record keeps the previous version until 0.15
//! removes those channels as an upgrade path.
//!
//! After the swap, the Claude `ecp` skill and `~/.claude/ECP.md` follow the
//! new release, but only while they still equal the copy this outgoing binary
//! embeds: a difference is kept, whether it is a local edit or a copy an older
//! release installed. The new binary does the install, so the files come from
//! its embedded copy, not this one's.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use clap::Args;
use ecp_core::EcpError;
use sha2::{Digest, Sha256};

use crate::commands::admin::claude::{has_ecp_import, skill_dir_in, ClaudeSkillTarget};
use crate::commands::admin::doctor::checks::install_source::{
    InstallSource, CHANNELS, CHANNEL_SUNSET,
};
use crate::commands::admin::doctor::checks::version::{latest_release_version, parse_semver};
use crate::commands::admin::skill_source::{embedded, EmbeddedTree, SkillSource};
use crate::commands::admin::update_check;
use crate::git::safe_exec;
use ecp_core::registry::{resolve_home_ecp, FileLock};

const REPO: &str = "coseto6125/egent-code-plexus";
const BIN: &str = "ecp";
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(180);
const TOOL_TIMEOUT: Duration = Duration::from_secs(60);
const SKILL_REFRESH_ARGS: [&str; 5] = ["admin", "claude", "install", "skills", "ecp"];
const NO_CLAUDE_MD: &str = "--no-claude-md";

#[derive(Args, Debug, Clone)]
pub struct UpdateArgs {
    /// Report whether a newer release exists without installing it.
    #[arg(long, default_value_t = false)]
    pub check: bool,
}

pub fn run(args: UpdateArgs) -> Result<(), EcpError> {
    let local = env!("CARGO_PKG_VERSION");
    let local_parsed = parse_semver(local)
        .ok_or_else(|| EcpError::Output(format!("local version {local} is not semver")))?;
    let latest = latest_release_version().ok_or_else(|| {
        EcpError::Output("could not read the latest GitHub Release (network or curl)".into())
    })?;
    let latest_str = format!("{}.{}.{}", latest.0, latest.1, latest.2);

    if latest <= local_parsed {
        println!("ecp v{local} is up to date (latest release v{latest_str})");
        if !args.check {
            update_check::clear_available_notice();
        }
        return Ok(());
    }
    if args.check {
        println!(
            "ecp v{latest_str} is available (you have v{local}). Run `ecp update` to install it."
        );
        return Ok(());
    }

    let target = target_triple(std::env::consts::OS, std::env::consts::ARCH)
        .filter(|_| !cfg!(target_env = "musl"))
        .ok_or_else(|| {
            EcpError::Output(format!(
                "no prebuilt release for this build ({}/{}{}); build from source: cargo install --git https://github.com/{REPO} egent-code-plexus --bin ecp --locked",
                std::env::consts::OS,
                std::env::consts::ARCH,
                if cfg!(target_env = "musl") { ", musl" } else { "" }
            ))
        })?;
    let home_ecp = resolve_home_ecp();
    let _ = std::fs::create_dir_all(&home_ecp);
    let _one_at_a_time = FileLock::try_exclusive(&home_ecp.join(".update.lock"))
        .map_err(|_| EcpError::Output("another ecp update is running".into()))?;
    let exe = current_exe()?;
    let dir = exe
        .parent()
        .ok_or_else(|| EcpError::Output(format!("{} has no parent directory", exe.display())))?;
    println!("==> ecp v{local} -> v{latest_str} ({target})");

    // Staged next to the binary so the final rename stays on one filesystem,
    // and private: what passes the digest check must be what gets installed.
    let mut staging = tempfile::Builder::new();
    staging.prefix(".ecp-update-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        staging.permissions(std::fs::Permissions::from_mode(0o700));
    }
    let staging = staging.tempdir_in(dir).map_err(|e| {
        let hint = if e.kind() == std::io::ErrorKind::PermissionDenied {
            "; the install directory is not writable by you (try `sudo ecp update`)"
        } else {
            ""
        };
        EcpError::Output(format!(
            "create staging dir in {}: {e}{hint}",
            dir.display()
        ))
    })?;
    let asset = asset_name(&latest_str, target);
    let url = format!("https://github.com/{REPO}/releases/download/v{latest_str}/{asset}");
    let archive = staging.path().join(&asset);
    let sidecar = staging.path().join(format!("{asset}.sha256"));
    println!("==> downloading {url}");
    download(&url, &archive)?;
    download(&format!("{url}.sha256"), &sidecar)?;
    let expected = std::fs::read_to_string(&sidecar)
        .ok()
        .and_then(|body| parse_sha256_sidecar(&body))
        .ok_or_else(|| EcpError::Output(format!("{asset}.sha256 does not hold a sha256 digest")))?;
    verify_sha256(&archive, &expected)?;
    println!("==> sha256 ok");
    match verify_provenance(&archive)? {
        Provenance::Verified => println!("==> provenance ok (gh attestation verify)"),
        Provenance::Unchecked(why) => println!("==> provenance unchecked: {why}"),
    }

    let new_bin = extract(&archive, staging.path(), &latest_str, target)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&new_bin, std::fs::Permissions::from_mode(0o755));
    }
    let reported = version_of(&new_bin)?;
    if !version_matches(&reported, &latest_str) {
        return Err(EcpError::Output(format!(
            "downloaded binary reports `{reported}`, expected v{latest_str}; nothing was replaced"
        )));
    }
    replace_binary(&exe, &new_bin)?;
    update_check::record_self_update(&latest_str);
    update_check::clear_available_notice();
    println!("✓ ecp v{latest_str} installed -> {}", exe.display());
    for line in refresh_ecp_skill(&exe, ecp_core::registry::home_dir()) {
        println!("{line}");
    }

    let source = InstallSource::detect();
    if source != InstallSource::Unknown {
        println!(
            "note: this binary came from {source:?}; that package manager still records v{local}. \
             Upgrading through {CHANNELS} is removed in {CHANNEL_SUNSET}; `ecp update` is the upgrade path."
        );
    }
    for other in other_copies_on_path(std::env::var_os("PATH"), &exe) {
        println!(
            "note: another `ecp` on PATH resolves elsewhere: {} (a package-manager launcher is fine; a second copy stays at its old version)",
            other.display()
        );
    }
    Ok(())
}

/// Release target for this host, matching the matrix in `release.yml`.
pub(crate) fn target_triple(os: &str, arch: &str) -> Option<&'static str> {
    Some(match (os, arch) {
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        _ => return None,
    })
}

/// Top-level directory inside the release archive.
pub(crate) fn archive_dir(version: &str, target: &str) -> String {
    format!("{BIN}-v{version}-{target}")
}

/// Release asset file name: `.zip` for Windows, `.tar.gz` elsewhere.
pub(crate) fn asset_name(version: &str, target: &str) -> String {
    let ext = if target.contains("windows") {
        "zip"
    } else {
        "tar.gz"
    };
    format!("{}.{ext}", archive_dir(version, target))
}

fn bin_file_name() -> String {
    format!("{BIN}{}", std::env::consts::EXE_SUFFIX)
}

/// The file to replace: a symlink on PATH (npm `bin`, Homebrew `opt`) points
/// at the real binary, and that is the one a new release must land on.
fn current_exe() -> Result<PathBuf, EcpError> {
    let exe = crate::subprocess::self_exe()?;
    Ok(dunce::canonicalize(&exe).unwrap_or(exe))
}

/// `--max-time` caps one attempt; no `--retry`, because a retry restarts the
/// transfer from zero and would only run into the outer kill. A stalled
/// transfer (under 1 KiB/s for 30 s) fails fast instead.
fn download(url: &str, dest: &Path) -> Result<(), EcpError> {
    let mut cmd = Command::new("curl");
    // `-q` first: a `.curlrc` left over from debugging (`insecure`, a proxy)
    // must not shape what gets installed. HTTPS only, redirects too.
    cmd.args([
        "-q",
        "-sSfL",
        "--proto",
        "=https",
        "--proto-redir",
        "=https",
        "--max-filesize",
        "200M",
        "--max-time",
        "170",
        "--speed-limit",
        "1024",
        "--speed-time",
        "30",
        "-H",
        "User-Agent: ecp-update",
        "-o",
    ])
    .arg(dest)
    .arg(url);
    let out = run_tool(cmd, "curl", DOWNLOAD_TIMEOUT)?;
    if !out.status.success() {
        return Err(EcpError::Output(format!(
            "download failed: {url}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}

/// `output_with_timeout` returns `None` for a missing tool and for a kill at
/// `timeout` alike; the user needs to know which.
fn run_tool(cmd: Command, tool: &str, timeout: Duration) -> Result<std::process::Output, EcpError> {
    safe_exec::output_with_timeout(cmd, timeout).ok_or_else(|| {
        let missing = Command::new(tool)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_err();
        EcpError::Output(if missing {
            format!("{tool} is not installed; ecp update needs curl and tar on PATH")
        } else {
            format!("{tool} did not finish within {}s", timeout.as_secs())
        })
    })
}

#[derive(Debug)]
pub(crate) enum Provenance {
    Verified,
    Unchecked(String),
}

/// `gh attestation verify` binds the archive to the release workflow's
/// identity, which a swapped asset and sidecar cannot forge. A verifier that
/// runs and rejects stops the update; one that cannot run (no `gh`, or `gh`
/// not signed in, exit 4) is reported and skipped.
fn verify_provenance(archive: &Path) -> Result<Provenance, EcpError> {
    let mut cmd = Command::new("gh");
    cmd.args(["attestation", "verify"])
        .arg(archive)
        .args(["--owner", REPO.split('/').next().unwrap_or(REPO)]);
    let Some(out) = safe_exec::output_with_timeout(cmd, TOOL_TIMEOUT) else {
        return Ok(Provenance::Unchecked(
            "`gh` is not installed or did not finish; see the release notes for `gh attestation verify`".into(),
        ));
    };
    provenance_outcome(out.status.code(), &String::from_utf8_lossy(&out.stderr))
}

/// gh exits 4 when it needs `gh auth login`; every other failure is a verdict.
pub(crate) fn provenance_outcome(code: Option<i32>, stderr: &str) -> Result<Provenance, EcpError> {
    match code {
        Some(0) => Ok(Provenance::Verified),
        Some(4) => Ok(Provenance::Unchecked(
            "`gh` is not signed in (`gh auth login` enables provenance checks)".into(),
        )),
        _ => Err(EcpError::Output(format!(
            "provenance check failed, nothing was replaced: {}",
            stderr.trim()
        ))),
    }
}

/// First token of a `<hex>  <file>` sidecar, lowercased. `None` unless it is a
/// 64-digit hex string, so a GitHub error page never becomes an expected digest.
pub(crate) fn parse_sha256_sidecar(body: &str) -> Option<String> {
    let token = body.split_whitespace().next()?;
    (token.len() == 64 && token.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| token.to_ascii_lowercase())
}

pub(crate) fn verify_sha256(path: &Path, expected: &str) -> Result<(), EcpError> {
    let bytes = std::fs::read(path).map_err(io_err(path))?;
    let actual = hex::encode(Sha256::digest(&bytes));
    if actual != expected {
        return Err(EcpError::Output(format!(
            "sha256 mismatch for {}: expected {expected}, got {actual}",
            path.display()
        )));
    }
    Ok(())
}

/// Unpack `archive` into `into` and return the binary at the release layout
/// `<archive_dir>/<bin>`. bsdtar on Windows reads the zip asset the same way.
pub(crate) fn extract(
    archive: &Path,
    into: &Path,
    version: &str,
    target: &str,
) -> Result<PathBuf, EcpError> {
    let mut cmd = Command::new("tar");
    cmd.arg("-xf").arg(archive).arg("-C").arg(into);
    let out = run_tool(cmd, "tar", TOOL_TIMEOUT)?;
    if !out.status.success() {
        return Err(EcpError::Output(format!(
            "extract {}: {}",
            archive.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let bin = into
        .join(archive_dir(version, target))
        .join(bin_file_name());
    if !bin.is_file() {
        return Err(EcpError::Output(format!(
            "{} does not contain {}",
            archive.display(),
            bin.display()
        )));
    }
    Ok(bin)
}

/// Where the outgoing binary is parked: `<exe>.old`, or `<exe>.old.<pid>` when
/// a still-running process holds that name (Windows keeps a mapped executable
/// locked against unlink, not against rename).
#[cfg(any(windows, test))]
fn park_path(exe: &Path) -> PathBuf {
    let mut s: OsString = exe.as_os_str().to_owned();
    s.push(".old");
    let base = PathBuf::from(s);
    if std::fs::remove_file(&base).is_ok() || !base.exists() {
        return base;
    }
    let mut s = base.into_os_string();
    s.push(format!(".{}", std::process::id()));
    PathBuf::from(s)
}

/// Remove every parked copy the OS lets go of; one a running process still
/// maps stays until the update after it.
fn sweep_parked(exe: &Path) {
    let (Some(dir), Some(name)) = (exe.parent(), exe.file_name().and_then(|n| n.to_str())) else {
        return;
    };
    let prefix = format!("{name}.old");
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Swap `new_bin` into `exe`'s place. Unix renames over the running file in
/// one step. Windows parks the running file first and restores it if the
/// second rename fails; when even that fails, the error names the parked file
/// so the user can move it back by hand.
pub(crate) fn replace_binary(exe: &Path, new_bin: &Path) -> Result<(), EcpError> {
    #[cfg(not(windows))]
    std::fs::rename(new_bin, exe)
        .map_err(|e| EcpError::Output(format!("install {}: {e}", exe.display())))?;
    #[cfg(windows)]
    {
        let old = park_path(exe);
        std::fs::rename(exe, &old)
            .map_err(|e| EcpError::Output(format!("move aside {}: {e}", exe.display())))?;
        if let Err(e) = std::fs::rename(new_bin, exe) {
            return Err(EcpError::Output(match std::fs::rename(&old, exe) {
                Ok(()) => format!(
                    "install {}: {e}; the previous binary is back in place",
                    exe.display()
                ),
                Err(back) => format!(
                    "install {}: {e}; restoring the previous binary failed too ({back}): move {} back by hand",
                    exe.display(),
                    old.display()
                ),
            }));
        }
    }
    sweep_parked(exe);
    Ok(())
}

fn version_of(exe: &Path) -> Result<String, EcpError> {
    let mut cmd = Command::new(exe);
    cmd.arg("--version");
    let out = safe_exec::output_with_timeout(cmd, TOOL_TIMEOUT)
        .ok_or_else(|| EcpError::Output(format!("{} --version did not complete", exe.display())))?;
    if !out.status.success() {
        return Err(EcpError::Output(format!(
            "{} --version failed: {}",
            exe.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `ecp 0.13.3+abc1234` reports version 0.13.3; `0.13.30` does not.
pub(crate) fn version_matches(reported: &str, expected: &str) -> bool {
    reported
        .split_whitespace()
        .nth(1)
        .and_then(|v| v.strip_prefix(expected))
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('+') || rest.starts_with('-'))
}

/// Other `ecp` binaries on `PATH` that this update did not touch, so a shell
/// resolving to one of them keeps running the old version.
pub(crate) fn other_copies_on_path(path: Option<OsString>, exe: &Path) -> Vec<PathBuf> {
    let Some(path) = path else {
        return Vec::new();
    };
    let exe = dunce::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
    let name = bin_file_name();
    let mut seen = Vec::new();
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(&name);
        if !candidate.is_file() {
            continue;
        }
        let resolved = dunce::canonicalize(&candidate).unwrap_or(candidate);
        if resolved != exe && !seen.contains(&resolved) {
            seen.push(resolved);
        }
    }
    seen
}

/// `ECP.md` against the copy the outgoing binary shipped. Content that
/// matches but that `CLAUDE.md` does not import is `Absent`: the refresh must
/// not add the import, the same as when the file is missing. Differing
/// content is `Modified`. `Special` is a symlink or special file, which an
/// install never writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalState {
    Absent,
    Shipped,
    Modified,
    Special,
}

/// The installed skill tree. It has no `Absent`: `refresh_ecp_skill` returns
/// before classifying when the skill is not installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SkillState {
    Shipped,
    Modified,
    Special,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefreshNote {
    SkillModified,
    SkillSpecial,
    EcpMdModified,
    EcpMdSpecial,
    /// `with_command`: only the ownership blocks the refresh, so this line
    /// carries the command. Otherwise a skill note below it does.
    ForeignOwner {
        with_command: bool,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RefreshPlan {
    pub run: bool,
    pub no_claude_md: bool,
    pub notes: Vec<RefreshNote>,
}

/// Refresh only what still equals the shipped copy. `--no-claude-md` keeps
/// the install from overwriting an edited ECP.md or adding an `@ECP.md`
/// import the user removed or never had. `foreign_owner` (root over another
/// user's `~/.claude`) skips: the child would leave root-owned files there
/// that the user can no longer edit.
pub(crate) fn plan_ecp_refresh(
    skill: SkillState,
    ecp_md: LocalState,
    foreign_owner: bool,
) -> RefreshPlan {
    let mut notes = Vec::new();
    if foreign_owner {
        notes.push(RefreshNote::ForeignOwner {
            with_command: skill == SkillState::Shipped,
        });
    }
    match skill {
        SkillState::Shipped => {}
        SkillState::Modified => notes.push(RefreshNote::SkillModified),
        SkillState::Special => notes.push(RefreshNote::SkillSpecial),
    }
    match ecp_md {
        LocalState::Modified => notes.push(RefreshNote::EcpMdModified),
        LocalState::Special => notes.push(RefreshNote::EcpMdSpecial),
        LocalState::Absent | LocalState::Shipped => {}
    }
    RefreshPlan {
        run: skill == SkillState::Shipped && !foreign_owner,
        no_claude_md: ecp_md != LocalState::Shipped,
        notes,
    }
}

/// The manual form of the refresh, from the same argv the child gets.
fn skill_refresh_cmd(no_claude_md: bool) -> String {
    let mut cmd = format!("{BIN} {}", SKILL_REFRESH_ARGS.join(" "));
    if no_claude_md {
        cmd.push(' ');
        cmd.push_str(NO_CLAUDE_MD);
    }
    cmd
}

// The comparison is against the outgoing binary's copy, so an install from
// an older release differs too; the note must not call that a local edit.
const DIFFERS: &str =
    "differs from this release's copy (local edits, or installed by an older release)";
const OPAQUE: &str = "is a symlink or special file";
const OPAQUE_TREE: &str = "is, or contains, a symlink or special file";
const OUTSIDE_CHECKOUT: &str = "run it outside an ecp checkout";

impl RefreshNote {
    fn line(self, install: &EcpInstall, no_claude_md: bool) -> String {
        match self {
            RefreshNote::SkillModified => format!(
                "note: {} {DIFFERS} and was not refreshed; `{}` replaces it ({OUTSIDE_CHECKOUT})",
                install.skill_dir.display(),
                skill_refresh_cmd(no_claude_md)
            ),
            RefreshNote::SkillSpecial => format!(
                "note: {} {OPAQUE_TREE} and was kept; `{}` replaces it ({OUTSIDE_CHECKOUT})",
                install.skill_dir.display(),
                skill_refresh_cmd(no_claude_md)
            ),
            RefreshNote::EcpMdModified => format!(
                "note: {} {DIFFERS} and was kept ({NO_CLAUDE_MD})",
                install.ecp_md.display()
            ),
            RefreshNote::EcpMdSpecial => format!(
                "note: {} {OPAQUE} and was kept ({NO_CLAUDE_MD})",
                install.ecp_md.display()
            ),
            RefreshNote::ForeignOwner { with_command } => {
                let run = if with_command {
                    format!("`{}`", skill_refresh_cmd(no_claude_md))
                } else {
                    "the command below".to_owned()
                };
                format!(
                    "note: running as root, and {} belongs to another user, so the Claude ecp skill was not refreshed; run {run} as that user",
                    install.claude_home.display()
                )
            }
        }
    }
}

struct EcpInstall {
    claude_home: PathBuf,
    skill_dir: PathBuf,
    ecp_md: PathBuf,
}

impl EcpInstall {
    fn at(claude_home: PathBuf) -> Self {
        Self {
            skill_dir: skill_dir_in(&claude_home, ClaudeSkillTarget::Ecp),
            ecp_md: claude_home.join("ECP.md"),
            claude_home,
        }
    }
}

/// The `ecp` skill this outgoing binary embeds, never a repo checkout under
/// cwd: a checkout is not what the user was given.
fn shipped_ecp_skill() -> Result<SkillSource, EcpError> {
    embedded(EmbeddedTree::EcpSkill)
}

/// `symlink_metadata`, so a dangling `SKILL.md` symlink still counts as an
/// install, reported as special rather than ignored.
fn skill_installed(skill_dir: &Path) -> bool {
    !matches!(
        std::fs::symlink_metadata(skill_dir.join("SKILL.md")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound
    )
}

fn classify_ecp_install(
    install: &EcpInstall,
    shipped: &Path,
) -> Result<(SkillState, LocalState), EcpError> {
    Ok((
        skill_state(&install.skill_dir, shipped)?,
        ecp_md_state(install, shipped)?,
    ))
}

/// Names the path in an I/O error, so a permission failure says which file.
fn io_err(path: &Path) -> impl Fn(std::io::Error) -> EcpError + '_ {
    move |e| EcpError::Output(format!("read {}: {e}", path.display()))
}

fn is_os_junk(rel: &Path) -> bool {
    matches!(
        rel.file_name().and_then(|n| n.to_str()),
        Some(".DS_Store" | "Thumbs.db" | "desktop.ini")
    )
}

/// An install writes only regular files and directories. A symlink or a
/// special file is the user's own doing (`Special`); an extra directory, an
/// extra or missing file, or other content is `Modified`.
fn skill_state(skill_dir: &Path, shipped: &Path) -> Result<SkillState, EcpError> {
    if !std::fs::symlink_metadata(skill_dir)
        .map_err(io_err(skill_dir))?
        .is_dir()
    {
        return Ok(SkillState::Special);
    }
    let (Some(local), Some(ours)) = (regular_tree(skill_dir)?, regular_tree(shipped)?) else {
        return Ok(SkillState::Special);
    };
    if local.dirs.difference(&ours.dirs).next().is_some()
        || local
            .files
            .difference(&ours.files)
            .any(|rel| !is_os_junk(rel))
    {
        return Ok(SkillState::Modified);
    }
    for rel in &ours.files {
        if !local.files.contains(rel) {
            return Ok(SkillState::Modified);
        }
        let (local_file, shipped_file) = (skill_dir.join(rel), shipped.join(rel));
        if std::fs::read(&local_file).map_err(io_err(&local_file))?
            != std::fs::read(&shipped_file).map_err(io_err(&shipped_file))?
        {
            return Ok(SkillState::Modified);
        }
    }
    Ok(SkillState::Shipped)
}

#[derive(Default)]
struct RegularTree {
    files: BTreeSet<PathBuf>,
    dirs: BTreeSet<PathBuf>,
}

/// `None` when an entry under `root` is neither a regular file nor a
/// directory. `DirEntry::file_type` does not follow symlinks.
fn regular_tree(root: &Path) -> Result<Option<RegularTree>, EcpError> {
    let mut tree = RegularTree::default();
    let mut pending = vec![PathBuf::new()];
    while let Some(rel_dir) = pending.pop() {
        let dir = root.join(&rel_dir);
        for entry in std::fs::read_dir(&dir).map_err(io_err(&dir))? {
            let entry = entry.map_err(io_err(&dir))?;
            let kind = entry.file_type().map_err(io_err(&entry.path()))?;
            let rel = rel_dir.join(entry.file_name());
            if kind.is_dir() {
                tree.dirs.insert(rel.clone());
                pending.push(rel);
            } else if kind.is_file() {
                tree.files.insert(rel);
            } else {
                return Ok(None);
            }
        }
    }
    Ok(Some(tree))
}

fn ecp_md_state(install: &EcpInstall, shipped: &Path) -> Result<LocalState, EcpError> {
    // The file type comes first: reading a FIFO would block the update.
    match std::fs::symlink_metadata(&install.ecp_md) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(LocalState::Absent),
        Err(e) => return Err(io_err(&install.ecp_md)(e)),
        Ok(meta) if !meta.is_file() => return Ok(LocalState::Special),
        Ok(_) => {}
    }
    let shipped_md = shipped.join("ECP.md");
    let local = std::fs::read(&install.ecp_md).map_err(io_err(&install.ecp_md))?;
    if local != std::fs::read(&shipped_md).map_err(io_err(&shipped_md))? {
        return Ok(LocalState::Modified);
    }
    let claude_md = install.claude_home.join("CLAUDE.md");
    let imported = std::fs::metadata(&claude_md).is_ok_and(|m| m.is_file())
        && has_ecp_import(&std::fs::read_to_string(&claude_md).unwrap_or_default());
    Ok(if imported {
        LocalState::Shipped
    } else {
        LocalState::Absent
    })
}

/// `metadata` follows a symlinked `~/.claude` to the directory the child
/// writes into; `symlink_metadata` would report the link's own owner.
#[cfg(unix)]
fn owned_by_other_than(dir: &Path, uid: u32) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(dir).is_ok_and(|m| m.uid() != uid)
}

/// When sudo preserves HOME, the euid is root while `~/.claude` belongs to
/// the user.
#[cfg(unix)]
fn root_over_foreign_dir(dir: &Path) -> bool {
    // SAFETY: geteuid has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    euid == 0 && owned_by_other_than(dir, 0)
}

#[cfg(not(unix))]
fn root_over_foreign_dir(_dir: &Path) -> bool {
    false
}

/// The binary is already installed, so a failed refresh is a note, never a
/// failed update. The returned lines are what `run` prints.
fn refresh_ecp_skill(new_exe: &Path, home: Option<PathBuf>) -> Vec<String> {
    // One absolute home, passed to the child as HOME, keeps the comparison
    // and the child's install on the same directory: HOME may be relative,
    // and on Windows `home_dir` may have fallen back to USERPROFILE.
    let Some(Ok(home)) = home.map(std::path::absolute) else {
        return Vec::new();
    };
    let install = EcpInstall::at(home.join(".claude"));
    if !skill_installed(&install.skill_dir) {
        return Vec::new();
    }
    let foreign_owner = root_over_foreign_dir(&install.claude_home);
    let compared = shipped_ecp_skill().and_then(|s| classify_ecp_install(&install, s.path()));
    let (skill, ecp_md) = match compared {
        Ok(states) => states,
        Err(e) => {
            let mut lines: Vec<String> = foreign_owner
                .then(|| {
                    RefreshNote::ForeignOwner {
                        with_command: false,
                    }
                    .line(&install, true)
                })
                .into_iter()
                .collect();
            lines.push(format!(
                "note: could not compare the Claude ecp skill with the shipped copy ({e}); `{}` replaces the installed copy ({OUTSIDE_CHECKOUT})",
                skill_refresh_cmd(true)
            ));
            return lines;
        }
    };
    let plan = plan_ecp_refresh(skill, ecp_md, foreign_owner);
    let mut lines: Vec<String> = plan
        .notes
        .iter()
        .map(|note| note.line(&install, plan.no_claude_md))
        .collect();
    if plan.run {
        lines.push(
            match run_skill_refresh(new_exe, &home, plan.no_claude_md, TOOL_TIMEOUT) {
                Ok(()) => format!(
                    "✓ Claude skill ecp refreshed -> {}",
                    install.skill_dir.display()
                ),
                Err(e) => format!(
                    "note: refreshing the Claude ecp skill failed ({e}); run `{}`",
                    skill_refresh_cmd(plan.no_claude_md)
                ),
            },
        );
    }
    lines
}

/// Run `exe admin claude install skills ecp` from an empty scratch dir, so the
/// child resolves its own embedded copy rather than a repo checkout. Stdout
/// goes to null instead of through `output_with_timeout`: that helper reads
/// the pipe only after exit, and a full-skill diff (the tree is ~54 KB)
/// overflows the pipe buffer, blocking the child until the timeout kills it.
fn run_skill_refresh(
    exe: &Path,
    home: &Path,
    no_claude_md: bool,
    timeout: Duration,
) -> Result<(), EcpError> {
    let scratch =
        tempfile::tempdir().map_err(|e| EcpError::Output(format!("create temp dir: {e}")))?;
    let mut cmd = Command::new(exe);
    cmd.args(SKILL_REFRESH_ARGS)
        .env("HOME", home)
        .current_dir(scratch.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if no_claude_md {
        cmd.arg(NO_CLAUDE_MD);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| EcpError::Output(format!("spawn {}: {e}", exe.display())))?;
    let deadline = Instant::now() + timeout;
    while child.try_wait()?.is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(EcpError::Output(format!(
                "{} did not finish within {}s",
                exe.display(),
                timeout.as_secs()
            )));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err(EcpError::Output(format!(
            "{} exited with {}: {}",
            exe.display(),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_target_triple_maps_release_matrix_and_rejects_others() {
        assert_eq!(
            target_triple("linux", "x86_64"),
            Some("x86_64-unknown-linux-gnu")
        );
        assert_eq!(
            target_triple("linux", "aarch64"),
            Some("aarch64-unknown-linux-gnu")
        );
        assert_eq!(
            target_triple("macos", "aarch64"),
            Some("aarch64-apple-darwin")
        );
        assert_eq!(
            target_triple("windows", "x86_64"),
            Some("x86_64-pc-windows-msvc")
        );
        assert_eq!(target_triple("windows", "aarch64"), None);
        assert_eq!(target_triple("freebsd", "x86_64"), None);
    }

    #[test]
    fn test_asset_name_matches_release_yml_layout() {
        // Contract: `release.yml` packages `${BIN}-${tag}-${target}.tar.gz`
        // (zip on Windows) and install.sh downloads that same name.
        assert_eq!(
            asset_name("0.13.3", "x86_64-unknown-linux-gnu"),
            "ecp-v0.13.3-x86_64-unknown-linux-gnu.tar.gz"
        );
        assert_eq!(
            asset_name("0.13.3", "x86_64-pc-windows-msvc"),
            "ecp-v0.13.3-x86_64-pc-windows-msvc.zip"
        );
    }

    #[test]
    fn test_parse_sha256_sidecar_takes_first_hex_token_only() {
        let digest = "A".repeat(64);
        assert_eq!(
            parse_sha256_sidecar(&format!("{digest}  ecp-v0.13.3-x.tar.gz\n")),
            Some("a".repeat(64))
        );
        assert_eq!(parse_sha256_sidecar(""), None);
        assert_eq!(parse_sha256_sidecar("Not Found"), None);
        assert_eq!(parse_sha256_sidecar(&"a".repeat(63)), None);
        assert_eq!(parse_sha256_sidecar(&format!("{}g", "a".repeat(63))), None);
    }

    #[test]
    fn test_verify_sha256_accepts_matching_digest_and_rejects_other() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("asset");
        std::fs::write(&file, b"payload").unwrap();
        let good = hex::encode(Sha256::digest(b"payload"));
        assert!(verify_sha256(&file, &good).is_ok());
        let err = verify_sha256(&file, &"0".repeat(64))
            .unwrap_err()
            .to_string();
        assert!(err.contains("sha256 mismatch"), "{err}");
        assert!(verify_sha256(&dir.path().join("missing"), &good).is_err());
    }

    #[test]
    fn test_extract_returns_binary_at_release_layout() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let name = archive_dir("9.9.9", "x86_64-unknown-linux-gnu");
        std::fs::create_dir_all(src.join(&name)).unwrap();
        std::fs::write(src.join(&name).join(bin_file_name()), b"#!/bin/sh\n").unwrap();
        let archive = dir.path().join("asset.tar.gz");
        let status = Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&src)
            .arg(&name)
            .status()
            .unwrap();
        assert!(status.success());

        let out = dir.path().join("out");
        std::fs::create_dir(&out).unwrap();
        let bin = extract(&archive, &out, "9.9.9", "x86_64-unknown-linux-gnu").unwrap();
        assert_eq!(bin, out.join(&name).join(bin_file_name()));
        assert!(bin.is_file());

        // A different version in the name means the layout does not match.
        let err = extract(&archive, &out, "9.9.8", "x86_64-unknown-linux-gnu")
            .unwrap_err()
            .to_string();
        assert!(err.contains("does not contain"), "{err}");
    }

    #[test]
    fn test_replace_binary_swaps_content_and_sweeps_old_copy() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join(bin_file_name());
        let new_bin = dir.path().join("staged");
        std::fs::write(&exe, b"old").unwrap();
        std::fs::write(&new_bin, b"new").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&new_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // Leftovers from earlier updates (plain and pid-suffixed) must neither
        // block the swap nor survive it.
        let stale_plain = dir.path().join(format!("{}.old", bin_file_name()));
        let stale_pid = dir.path().join(format!("{}.old.4242", bin_file_name()));
        std::fs::write(&stale_plain, b"stale").unwrap();
        std::fs::write(&stale_pid, b"stale").unwrap();

        replace_binary(&exe, &new_bin).unwrap();

        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
        assert!(!new_bin.exists());
        assert!(!stale_plain.exists());
        assert!(!stale_pid.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&exe).unwrap().permissions().mode() & 0o111,
                0o111
            );
        }
    }

    #[test]
    fn test_replace_binary_restores_previous_when_new_file_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join(bin_file_name());
        std::fs::write(&exe, b"old").unwrap();

        let err = replace_binary(&exe, &dir.path().join("absent"))
            .unwrap_err()
            .to_string();

        assert!(err.contains("install "), "{err}");
        assert_eq!(std::fs::read(&exe).unwrap(), b"old");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn test_provenance_outcome_maps_exit_codes() {
        assert!(matches!(
            provenance_outcome(Some(0), ""),
            Ok(Provenance::Verified)
        ));
        assert!(matches!(
            provenance_outcome(
                Some(4),
                "To get started with GitHub CLI, please run: gh auth login"
            ),
            Ok(Provenance::Unchecked(_))
        ));
        let err = provenance_outcome(Some(1), "no attestations found")
            .unwrap_err()
            .to_string();
        assert!(err.contains("nothing was replaced"), "{err}");
        assert!(provenance_outcome(None, "killed").is_err());
    }

    #[test]
    fn test_version_matches_accepts_build_suffix_and_rejects_longer_version() {
        assert!(version_matches("ecp 0.13.3", "0.13.3"));
        assert!(version_matches("ecp 0.13.3+2d65ddf", "0.13.3"));
        assert!(version_matches("ecp 0.13.3-rc1\n", "0.13.3"));
        assert!(!version_matches("ecp 0.13.30", "0.13.3"));
        assert!(!version_matches("ecp 0.13.2+abc", "0.13.3"));
        assert!(!version_matches("", "0.13.3"));
        assert!(!version_matches("0.13.3", "0.13.3"));
    }

    #[test]
    fn test_park_path_uses_plain_name_and_reclaims_a_removable_leftover() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join(bin_file_name());
        let plain = dir.path().join(format!("{}.old", bin_file_name()));
        assert_eq!(park_path(&exe), plain);
        std::fs::write(&plain, b"stale").unwrap();
        assert_eq!(park_path(&exe), plain);
        assert!(!plain.exists());
    }

    /// Unix cannot hold a file against unlink the way Windows does, so the
    /// fallback branch is driven by a read-only parent directory instead.
    #[cfg(unix)]
    #[test]
    fn test_park_path_falls_back_to_pid_suffix_when_old_cannot_be_removed() {
        use std::os::unix::fs::PermissionsExt;
        // root ignores directory modes, so the fallback cannot be provoked.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join(bin_file_name());
        let plain = dir.path().join(format!("{}.old", bin_file_name()));
        std::fs::write(&plain, b"held").unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();

        let parked = park_path(&exe);

        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            parked,
            dir.path()
                .join(format!("{}.old.{}", bin_file_name(), std::process::id()))
        );
        assert!(plain.exists());
    }

    #[test]
    fn test_other_copies_on_path_lists_binaries_that_are_not_the_running_one() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b, c) = (
            dir.path().join("a"),
            dir.path().join("b"),
            dir.path().join("c"),
        );
        for d in [&a, &b, &c] {
            std::fs::create_dir(d).unwrap();
        }
        let exe = a.join(bin_file_name());
        std::fs::write(&exe, b"x").unwrap();
        std::fs::write(b.join(bin_file_name()), b"y").unwrap();
        // `c` holds no ecp; a directory that is listed twice counts once.
        let path = std::env::join_paths([&a, &b, &c, &b, &dir.path().join("missing")]).unwrap();

        let others = other_copies_on_path(Some(path), &exe);

        assert_eq!(
            others,
            vec![dunce::canonicalize(b.join(bin_file_name())).unwrap()]
        );
        assert!(other_copies_on_path(None, &exe).is_empty());
    }

    const ECP_MD_NOTES: [(LocalState, Option<RefreshNote>); 4] = [
        (LocalState::Absent, None),
        (LocalState::Shipped, None),
        (LocalState::Modified, Some(RefreshNote::EcpMdModified)),
        (LocalState::Special, Some(RefreshNote::EcpMdSpecial)),
    ];

    #[test]
    fn test_plan_ecp_refresh_unmodified_skill_and_shipped_ecp_md_refreshes_both() {
        assert_eq!(
            plan_ecp_refresh(SkillState::Shipped, LocalState::Shipped, false),
            RefreshPlan {
                run: true,
                no_claude_md: false,
                notes: vec![]
            }
        );
    }

    #[test]
    fn test_plan_ecp_refresh_unmodified_skill_without_ecp_md_passes_no_claude_md() {
        assert_eq!(
            plan_ecp_refresh(SkillState::Shipped, LocalState::Absent, false),
            RefreshPlan {
                run: true,
                no_claude_md: true,
                notes: vec![]
            }
        );
    }

    #[test]
    fn test_plan_ecp_refresh_unmodified_skill_and_edited_ecp_md_keeps_it_with_note() {
        assert_eq!(
            plan_ecp_refresh(SkillState::Shipped, LocalState::Modified, false),
            RefreshPlan {
                run: true,
                no_claude_md: true,
                notes: vec![RefreshNote::EcpMdModified]
            }
        );
    }

    #[test]
    fn test_plan_ecp_refresh_edited_or_special_skill_skips_with_note() {
        for (skill, note) in [
            (SkillState::Modified, RefreshNote::SkillModified),
            (SkillState::Special, RefreshNote::SkillSpecial),
        ] {
            for (ecp_md, md_note) in ECP_MD_NOTES {
                let mut notes = vec![note];
                notes.extend(md_note);
                assert_eq!(
                    plan_ecp_refresh(skill, ecp_md, false),
                    RefreshPlan {
                        run: false,
                        no_claude_md: ecp_md != LocalState::Shipped,
                        notes
                    },
                    "{skill:?} {ecp_md:?}"
                );
            }
        }
    }

    #[test]
    fn test_plan_ecp_refresh_root_over_foreign_home_skips_with_foreign_note_first() {
        for (skill, skill_note) in [
            (SkillState::Shipped, None),
            (SkillState::Modified, Some(RefreshNote::SkillModified)),
            (SkillState::Special, Some(RefreshNote::SkillSpecial)),
        ] {
            for (ecp_md, md_note) in ECP_MD_NOTES {
                let mut notes = vec![RefreshNote::ForeignOwner {
                    with_command: skill_note.is_none(),
                }];
                notes.extend(skill_note);
                notes.extend(md_note);
                assert_eq!(
                    plan_ecp_refresh(skill, ecp_md, true),
                    RefreshPlan {
                        run: false,
                        no_claude_md: ecp_md != LocalState::Shipped,
                        notes
                    },
                    "{skill:?} {ecp_md:?}"
                );
            }
        }
    }

    #[test]
    fn test_refresh_note_line_names_path_and_manual_command() {
        let install = EcpInstall::at(PathBuf::from("/h/.claude"));
        let skill = RefreshNote::SkillModified.line(&install, false);
        assert!(skill.starts_with("note: "), "{skill}");
        assert!(skill.contains(&install.skill_dir.display().to_string()));
        assert!(skill.contains("installed by an older release"), "{skill}");
        assert!(
            skill.contains(&format!("`{}`", skill_refresh_cmd(false))),
            "{skill}"
        );
        let special = RefreshNote::SkillSpecial.line(&install, false);
        assert!(
            special.contains("is, or contains, a symlink or special file and was kept"),
            "{special}"
        );
        assert!(!special.contains("differs"), "{special}");
        assert!(!special.contains("this release's copy"), "{special}");
        assert!(
            special.contains(&format!("`{}`", skill_refresh_cmd(false))),
            "{special}"
        );
        let md_special = RefreshNote::EcpMdSpecial.line(&install, true);
        assert!(
            md_special.contains("is a symlink or special file and was kept"),
            "{md_special}"
        );
        assert!(!md_special.contains("differs"), "{md_special}");
        let md = RefreshNote::EcpMdModified.line(&install, true);
        assert!(md.starts_with("note: "), "{md}");
        assert!(md.contains(&install.ecp_md.display().to_string()));
        assert!(md.contains("installed by an older release"), "{md}");
        assert!(
            !md.contains(&format!("`{}`", skill_refresh_cmd(false))),
            "{md}"
        );
        let root = RefreshNote::ForeignOwner { with_command: true }.line(&install, false);
        assert!(root.contains(&install.claude_home.display().to_string()));
        assert!(
            root.contains(&format!("`{}`", skill_refresh_cmd(false))),
            "{root}"
        );
        let root_bare = RefreshNote::ForeignOwner {
            with_command: false,
        }
        .line(&install, false);
        assert!(
            root_bare.contains("run the command below as that user"),
            "{root_bare}"
        );
        assert!(
            !root_bare.contains(&skill_refresh_cmd(false)),
            "{root_bare}"
        );
    }

    #[test]
    fn test_skill_refresh_cmd_renders_argv_with_and_without_flag() {
        assert_eq!(
            skill_refresh_cmd(false),
            "ecp admin claude install skills ecp"
        );
        assert_eq!(
            skill_refresh_cmd(true),
            "ecp admin claude install skills ecp --no-claude-md"
        );
    }

    #[test]
    fn test_refresh_note_line_ecp_md_absent_or_unimported_command_carries_no_claude_md() {
        let notes_for = |claude_home: &Path| {
            let install = EcpInstall::at(claude_home.to_path_buf());
            let (_, ecp_md) = classify(claude_home);
            let plan = plan_ecp_refresh(SkillState::Modified, ecp_md, false);
            plan.notes
                .iter()
                .map(|n| n.line(&install, plan.no_claude_md))
                .collect::<Vec<_>>()
        };
        let with_flag = format!("`{}`", skill_refresh_cmd(true));
        let without_flag = format!("`{}`", skill_refresh_cmd(false));

        let home = tempfile::tempdir().unwrap();
        install_shipped(home.path());
        assert_eq!(
            notes_for(home.path()),
            vec![RefreshNote::SkillModified.line(&EcpInstall::at(home.path().to_path_buf()), false)]
        );
        assert!(notes_for(home.path())[0].contains(&without_flag));

        std::fs::write(home.path().join("CLAUDE.md"), "# mine\n").unwrap();
        let unimported = notes_for(home.path());
        assert!(unimported[0].contains(&with_flag), "{unimported:?}");

        std::fs::remove_file(home.path().join("ECP.md")).unwrap();
        let absent = notes_for(home.path());
        assert!(absent[0].contains(&with_flag), "{absent:?}");
    }

    /// Install the shipped skill, ECP.md and the `@ECP.md` import the way
    /// `ecp admin claude install skills ecp` does from a binary-only install.
    fn install_shipped(claude_home: &Path) {
        use crate::commands::admin::claude::{inject_ecp_import_at, install_skills_at};
        let empty = tempfile::tempdir().unwrap();
        install_skills_at(ClaudeSkillTarget::Ecp, false, empty.path(), claude_home).unwrap();
        let shipped = shipped_ecp_skill().unwrap();
        inject_ecp_import_at(claude_home, &shipped.path().join("ECP.md"), false).unwrap();
    }

    fn classify(claude_home: &Path) -> (SkillState, LocalState) {
        let shipped = shipped_ecp_skill().unwrap();
        classify_ecp_install(&EcpInstall::at(claude_home.to_path_buf()), shipped.path()).unwrap()
    }

    fn skill_dir(claude_home: &Path) -> PathBuf {
        skill_dir_in(claude_home, ClaudeSkillTarget::Ecp)
    }

    #[test]
    fn test_skill_installed_no_skill_dir_returns_false() {
        let home = tempfile::tempdir().unwrap();
        assert!(!skill_installed(&skill_dir(home.path())));
    }

    #[test]
    fn test_skill_installed_dir_without_skill_md_returns_false() {
        let home = tempfile::tempdir().unwrap();
        let dir = skill_dir(home.path());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("notes.md"), "mine\n").unwrap();
        assert!(!skill_installed(&dir));
        install_shipped(home.path());
        assert!(skill_installed(&dir));
    }

    #[test]
    fn test_classify_ecp_install_shipped_copy_installed_twice_reports_unmodified() {
        // A second refresh (an update run again) must see its own output as
        // unmodified, not as a local edit.
        let home = tempfile::tempdir().unwrap();
        install_shipped(home.path());
        assert_eq!(
            classify(home.path()),
            (SkillState::Shipped, LocalState::Shipped)
        );
        install_shipped(home.path());
        assert_eq!(
            classify(home.path()),
            (SkillState::Shipped, LocalState::Shipped)
        );
        let claude_md = std::fs::read_to_string(home.path().join("CLAUDE.md")).unwrap();
        assert_eq!(claude_md.matches("@ECP.md").count(), 1);
    }

    #[test]
    fn test_classify_ecp_install_edited_skill_reports_modified() {
        for edit in ["appended\n", "", "  \n\t\n"] {
            let home = tempfile::tempdir().unwrap();
            install_shipped(home.path());
            let skill_md = skill_dir(home.path()).join("SKILL.md");
            if edit.trim().is_empty() {
                std::fs::write(&skill_md, edit).unwrap();
            } else {
                let mut body = std::fs::read_to_string(&skill_md).unwrap();
                body.push_str(edit);
                std::fs::write(&skill_md, body).unwrap();
            }
            assert_eq!(
                classify(home.path()),
                (SkillState::Modified, LocalState::Shipped),
                "{edit:?}"
            );
        }
    }

    #[test]
    fn test_classify_ecp_install_added_file_in_skill_reports_modified() {
        let home = tempfile::tempdir().unwrap();
        install_shipped(home.path());
        std::fs::write(skill_dir(home.path()).join("local.md"), "mine\n").unwrap();
        assert_eq!(classify(home.path()).0, SkillState::Modified);
    }

    #[test]
    fn test_classify_ecp_install_removed_file_in_skill_reports_modified() {
        let home = tempfile::tempdir().unwrap();
        install_shipped(home.path());
        std::fs::remove_file(skill_dir(home.path()).join("ECP.md")).unwrap();
        assert_eq!(classify(home.path()).0, SkillState::Modified);
    }

    #[test]
    fn test_classify_ecp_install_empty_extra_dir_reports_modified() {
        let home = tempfile::tempdir().unwrap();
        install_shipped(home.path());
        std::fs::create_dir(skill_dir(home.path()).join("drafts")).unwrap();
        assert_eq!(classify(home.path()).0, SkillState::Modified);
    }

    #[test]
    fn test_classify_ecp_install_os_junk_files_report_unmodified() {
        let home = tempfile::tempdir().unwrap();
        install_shipped(home.path());
        let dir = skill_dir(home.path());
        for junk in [".DS_Store", "Thumbs.db", "desktop.ini"] {
            std::fs::write(dir.join(junk), "x").unwrap();
            std::fs::write(dir.join("guides").join(junk), "").unwrap();
        }
        assert_eq!(
            classify(home.path()),
            (SkillState::Shipped, LocalState::Shipped)
        );
    }

    #[test]
    fn test_classify_ecp_install_ecp_md_states() {
        let home = tempfile::tempdir().unwrap();
        install_shipped(home.path());
        let ecp_md = home.path().join("ECP.md");
        let claude_md = home.path().join("CLAUDE.md");

        std::fs::write(&claude_md, "# mine\n").unwrap();
        assert_eq!(classify(home.path()).1, LocalState::Absent);
        std::fs::remove_file(&claude_md).unwrap();
        assert_eq!(classify(home.path()).1, LocalState::Absent);

        for edited in ["", " \n\n", "my own guidance\n"] {
            std::fs::write(&ecp_md, edited).unwrap();
            assert_eq!(classify(home.path()).1, LocalState::Modified, "{edited:?}");
        }
        std::fs::remove_file(&ecp_md).unwrap();
        assert_eq!(classify(home.path()).1, LocalState::Absent);
    }

    #[cfg(unix)]
    #[test]
    fn test_classify_ecp_install_symlinked_ecp_md_reports_special() {
        // Identical content behind a symlink: a refresh would replace the
        // link with a file, so the link is the user's and is kept.
        let home = tempfile::tempdir().unwrap();
        install_shipped(home.path());
        let ecp_md = home.path().join("ECP.md");
        let target = home.path().join("ECP.dotfiles.md");
        std::fs::rename(&ecp_md, &target).unwrap();
        std::os::unix::fs::symlink(&target, &ecp_md).unwrap();
        assert_eq!(
            classify(home.path()),
            (SkillState::Shipped, LocalState::Special)
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_classify_ecp_install_symlink_inside_skill_reports_special() {
        let home = tempfile::tempdir().unwrap();
        install_shipped(home.path());
        let dir = skill_dir(home.path());
        std::os::unix::fs::symlink(dir.join("SKILL.md"), dir.join("extra.md")).unwrap();
        assert_eq!(classify(home.path()).0, SkillState::Special);
    }

    #[cfg(unix)]
    #[test]
    fn test_classify_ecp_install_symlinked_shipped_file_reports_special() {
        let home = tempfile::tempdir().unwrap();
        install_shipped(home.path());
        let skill_md = skill_dir(home.path()).join("SKILL.md");
        let target = home.path().join("SKILL.copy.md");
        std::fs::rename(&skill_md, &target).unwrap();
        std::os::unix::fs::symlink(&target, &skill_md).unwrap();
        assert_eq!(classify(home.path()).0, SkillState::Special);
    }

    #[cfg(unix)]
    #[test]
    fn test_classify_ecp_install_symlinked_skill_dir_reports_special() {
        let home = tempfile::tempdir().unwrap();
        install_shipped(home.path());
        let dir = skill_dir(home.path());
        let target = home.path().join("ecp-fork");
        std::fs::rename(&dir, &target).unwrap();
        std::os::unix::fs::symlink(&target, &dir).unwrap();
        assert!(skill_installed(&dir));
        assert_eq!(classify(home.path()).0, SkillState::Special);
    }

    #[cfg(unix)]
    fn mkfifo(path: &Path) {
        use std::os::unix::ffi::OsStrExt;
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: `c_path` is NUL-terminated and outlives the call.
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o644) }, 0);
    }

    /// Run `f` on a thread, so a regression that opens a FIFO fails the test
    /// instead of hanging it.
    #[cfg(unix)]
    fn within_deadline<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        rx.recv_timeout(Duration::from_secs(10))
            .expect("blocked: a FIFO was opened")
    }

    #[cfg(unix)]
    #[test]
    fn test_classify_ecp_install_fifo_ecp_md_reports_special_without_blocking() {
        let home = tempfile::tempdir().unwrap();
        install_shipped(home.path());
        let ecp_md = home.path().join("ECP.md");
        std::fs::remove_file(&ecp_md).unwrap();
        mkfifo(&ecp_md);
        let claude_home = home.path().to_path_buf();
        let state = within_deadline(move || classify(&claude_home));
        assert_eq!(state, (SkillState::Shipped, LocalState::Special));
    }

    #[cfg(unix)]
    #[test]
    fn test_classify_ecp_install_fifo_inside_skill_reports_special_without_blocking() {
        let home = tempfile::tempdir().unwrap();
        install_shipped(home.path());
        mkfifo(&skill_dir(home.path()).join("pipe"));
        let claude_home = home.path().to_path_buf();
        let state = within_deadline(move || classify(&claude_home));
        assert_eq!(state.0, SkillState::Special);
    }

    #[test]
    fn test_run_skill_refresh_missing_exe_returns_spawn_error() {
        let home = tempfile::tempdir().unwrap();
        let err = run_skill_refresh(
            Path::new("/no/such/binary/ecp"),
            home.path(),
            false,
            TOOL_TIMEOUT,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("spawn"), "{err}");
    }

    #[cfg(unix)]
    fn fake_exe(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let exe = dir.join("fake-ecp");
        std::fs::write(&exe, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        exe
    }

    #[cfg(unix)]
    #[test]
    fn test_run_skill_refresh_passes_flag_home_and_runs_outside_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let log = dir.path().join("log");
        let exe = fake_exe(
            dir.path(),
            &format!(
                "printf '%s\\n' \"$*\" \"$HOME\" >> '{}'; pwd >> '{}'",
                log.display(),
                log.display()
            ),
        );

        run_skill_refresh(&exe, home.path(), true, TOOL_TIMEOUT).unwrap();
        run_skill_refresh(&exe, home.path(), false, TOOL_TIMEOUT).unwrap();

        let logged = std::fs::read_to_string(&log).unwrap();
        let lines: Vec<&str> = logged.lines().collect();
        assert_eq!(lines.len(), 6, "{logged}");
        assert_eq!(lines[0], "admin claude install skills ecp --no-claude-md");
        assert_eq!(lines[3], "admin claude install skills ecp");
        let cwd = std::env::current_dir().unwrap();
        for i in [0, 3] {
            assert_eq!(Path::new(lines[i + 1]), home.path());
            assert_ne!(Path::new(lines[i + 2]), cwd);
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_run_skill_refresh_nonzero_exit_returns_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let exe = fake_exe(dir.path(), "echo boom >&2; exit 3");
        let err = run_skill_refresh(&exe, dir.path(), false, TOOL_TIMEOUT)
            .unwrap_err()
            .to_string();
        assert!(err.contains("boom"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn test_run_skill_refresh_hung_child_times_out() {
        let dir = tempfile::tempdir().unwrap();
        let exe = fake_exe(dir.path(), "exec sleep 30");
        let started = Instant::now();
        let err = run_skill_refresh(&exe, dir.path(), false, Duration::from_millis(200))
            .unwrap_err()
            .to_string();
        assert!(err.contains("did not finish"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[cfg(unix)]
    #[test]
    fn test_run_skill_refresh_large_stdout_does_not_block() {
        // More than a pipe buffer (64 KiB on Linux) of stdout, as a
        // full-skill diff prints.
        let dir = tempfile::tempdir().unwrap();
        let exe = fake_exe(dir.path(), "head -c 300000 /dev/zero");
        run_skill_refresh(&exe, dir.path(), false, Duration::from_secs(20)).unwrap();
    }

    #[cfg(unix)]
    fn marking_exe(bin: &Path, rest: &str) -> (PathBuf, PathBuf) {
        let ran = bin.join("ran");
        let exe = fake_exe(bin, &format!("touch '{}'; {rest}", ran.display()));
        (exe, ran)
    }

    #[cfg(unix)]
    #[test]
    fn test_refresh_ecp_skill_without_home_never_runs_child() {
        let bin = tempfile::tempdir().unwrap();
        let (exe, ran) = marking_exe(bin.path(), "");
        assert!(refresh_ecp_skill(&exe, None).is_empty());
        assert!(!ran.exists());
    }

    #[cfg(unix)]
    #[test]
    fn test_refresh_ecp_skill_skill_not_installed_never_reads_ecp_md_or_runs_child() {
        let home = tempfile::tempdir().unwrap();
        let claude_home = home.path().join(".claude");
        std::fs::create_dir_all(&claude_home).unwrap();
        mkfifo(&claude_home.join("ECP.md"));
        let bin = tempfile::tempdir().unwrap();
        let (exe, ran) = marking_exe(bin.path(), "");

        let home_path = home.path().to_path_buf();
        let lines = within_deadline(move || refresh_ecp_skill(&exe, Some(home_path)));

        assert!(lines.is_empty(), "{lines:?}");
        assert!(!ran.exists());
    }

    #[cfg(unix)]
    #[test]
    fn test_refresh_ecp_skill_relative_home_child_gets_absolute_home() {
        let home = tempfile::tempdir().unwrap();
        install_shipped(&home.path().join(".claude"));
        let cwd = std::env::current_dir().unwrap();
        let up: PathBuf = cwd.components().skip(1).map(|_| "..").collect();
        let relative = up.join(home.path().strip_prefix("/").unwrap());
        assert!(relative.is_relative());
        let bin = tempfile::tempdir().unwrap();
        let log = bin.path().join("home");
        let (exe, ran) = marking_exe(
            bin.path(),
            &format!("printf '%s' \"$HOME\" > '{}'", log.display()),
        );

        let lines = refresh_ecp_skill(&exe, Some(relative));

        assert!(lines.iter().any(|l| l.starts_with("✓")), "{lines:?}");
        assert!(ran.exists(), "the comparison missed the relative home");
        let child_home = PathBuf::from(std::fs::read_to_string(&log).unwrap());
        assert!(child_home.is_absolute(), "{}", child_home.display());
        assert_eq!(
            dunce::canonicalize(&child_home).unwrap(),
            dunce::canonicalize(home.path()).unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_refresh_ecp_skill_child_failure_runs_child_and_returns() {
        let home = tempfile::tempdir().unwrap();
        let claude_home = home.path().join(".claude");
        install_shipped(&claude_home);
        let bin = tempfile::tempdir().unwrap();
        let (exe, ran) = marking_exe(bin.path(), "echo boom >&2; exit 1");

        let lines = refresh_ecp_skill(&exe, Some(home.path().to_path_buf()));

        assert!(ran.exists(), "the child never ran");
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].contains("failed") && lines[0].contains("boom"),
            "{lines:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_refresh_ecp_skill_edited_skill_never_runs_child() {
        let home = tempfile::tempdir().unwrap();
        let claude_home = home.path().join(".claude");
        install_shipped(&claude_home);
        std::fs::write(skill_dir(&claude_home).join("SKILL.md"), "mine\n").unwrap();
        let bin = tempfile::tempdir().unwrap();
        let (exe, ran) = marking_exe(bin.path(), "");

        let lines = refresh_ecp_skill(&exe, Some(home.path().to_path_buf()));

        assert!(!ran.exists());
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("differs"), "{lines:?}");
    }

    #[cfg(unix)]
    #[test]
    fn test_refresh_ecp_skill_unreadable_skill_subdir_prints_one_command_and_never_runs_child() {
        use std::os::unix::fs::PermissionsExt;
        // SAFETY: geteuid has no preconditions and cannot fail.
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("skipped: root reads a mode 000 directory");
            return;
        }
        let home = tempfile::tempdir().unwrap();
        let claude_home = home.path().join(".claude");
        install_shipped(&claude_home);
        let locked = skill_dir(&claude_home).join("guides");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let bin = tempfile::tempdir().unwrap();
        let (exe, ran) = marking_exe(bin.path(), "");

        let lines = refresh_ecp_skill(&exe, Some(home.path().to_path_buf()));

        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!ran.exists(), "the child ran");
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("could not compare"), "{lines:?}");
        assert!(
            lines[0].contains(&format!("`{}`", skill_refresh_cmd(true))),
            "{lines:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_owned_by_other_than_follows_symlinked_dir() {
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: geteuid has no preconditions and cannot fail.
        let me = unsafe { libc::geteuid() };
        assert!(!owned_by_other_than(dir.path(), me));
        assert!(owned_by_other_than(dir.path(), me + 1));
        assert!(!owned_by_other_than(&dir.path().join("missing"), me));

        // The link itself is ours; only `metadata`, which follows it, sees the
        // target's owner. `/` is not root-owned everywhere (WSL gives it to the
        // first user), so pick a target this host gives to someone else.
        use std::os::unix::fs::MetadataExt;
        let Some(foreign) = ["/usr", "/etc", "/proc", "/"]
            .into_iter()
            .find(|p| std::fs::metadata(p).is_ok_and(|m| m.uid() != me))
        else {
            eprintln!("skipped: no directory owned by another uid on this host");
            return;
        };
        let link = dir.path().join("claude-link");
        std::os::unix::fs::symlink(foreign, &link).unwrap();
        assert!(owned_by_other_than(&link, me));
    }
}
