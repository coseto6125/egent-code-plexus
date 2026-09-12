//! `ecp update` — replace the running binary with the latest GitHub Release.
//!
//! Download and extraction go through `curl` and `tar`, which every supported
//! platform ships (Windows 10 1803+ carries both, and its bsdtar reads zip), so
//! the binary gains no HTTP or archive dependency. The release's `.sha256`
//! sidecar is verified before anything is swapped.
//!
//! Replacement is two renames inside the binary's own directory: the running
//! file moves aside to `<exe>.old`, the new file moves into place. Unix keeps
//! the old inode alive for this process; Windows lets a running executable be
//! renamed but not unlinked, so the `.old` file is swept on the next update.
//!
//! Channel installs (npm / uv / pip / brew / cargo) are replaced the same way;
//! the package manager's own record keeps the previous version until 0.15
//! removes those channels as an upgrade path.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use clap::Args;
use ecp_core::EcpError;
use sha2::{Digest, Sha256};

use crate::commands::admin::doctor::checks::install_source::InstallSource;
use crate::commands::admin::doctor::checks::version::{latest_published_version, parse_semver};
use crate::commands::admin::update_check;
use crate::git::safe_exec;

const REPO: &str = "coseto6125/egent-code-plexus";
const BIN: &str = "ecp";
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(180);
const TOOL_TIMEOUT: Duration = Duration::from_secs(60);

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
    let latest = latest_published_version().ok_or_else(|| {
        EcpError::Output("could not read the latest GitHub Release (network, curl, or git)".into())
    })?;
    let latest_str = format!("{}.{}.{}", latest.0, latest.1, latest.2);

    if latest <= local_parsed {
        println!("ecp v{local} is up to date (latest release v{latest_str})");
        update_check::clear_notification();
        return Ok(());
    }
    if args.check {
        println!(
            "ecp v{latest_str} is available (you have v{local}). Run `ecp update` to install it."
        );
        return Ok(());
    }

    let target = target_triple(std::env::consts::OS, std::env::consts::ARCH).ok_or_else(|| {
        EcpError::Output(format!(
            "no prebuilt release for {}/{}; build from source: cargo install --git https://github.com/{REPO} egent-code-plexus --bin ecp --locked",
            std::env::consts::OS,
            std::env::consts::ARCH
        ))
    })?;
    let exe = current_exe()?;
    let dir = exe
        .parent()
        .ok_or_else(|| EcpError::Output(format!("{} has no parent directory", exe.display())))?;
    println!("==> ecp v{local} -> v{latest_str} ({target})");

    // Staged next to the binary so the final rename stays on one filesystem.
    let staging = tempfile::Builder::new()
        .prefix(".ecp-update-")
        .tempdir_in(dir)
        .map_err(|e| EcpError::Output(format!("create staging dir in {}: {e}", dir.display())))?;
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

    let new_bin = extract(&archive, staging.path(), &latest_str, target)?;
    replace_binary(&exe, &new_bin)?;
    let installed = version_of(&exe)?;
    if !installed.contains(&latest_str) {
        return Err(EcpError::Output(format!(
            "installed binary reports `{installed}`, expected v{latest_str}; the previous binary is at {}",
            old_path(&exe).display()
        )));
    }
    update_check::record_self_update(&latest_str);
    update_check::clear_notification();
    println!("✓ ecp v{latest_str} installed -> {}", exe.display());

    let source = InstallSource::detect();
    if source != InstallSource::Unknown {
        println!(
            "note: this binary came from {source:?}; that package manager still records v{local}. \
             Upgrading through npm / uv / pip / brew / cargo is removed in 0.15; `ecp update` is the upgrade path."
        );
    }
    for other in other_copies_on_path(std::env::var_os("PATH"), &exe) {
        println!(
            "warning: another ecp on PATH was not updated: {}",
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

fn current_exe() -> Result<PathBuf, EcpError> {
    let exe = std::env::current_exe()
        .map_err(|e| EcpError::Output(format!("locate running binary: {e}")))?;
    Ok(dunce::canonicalize(&exe).unwrap_or(exe))
}

fn download(url: &str, dest: &Path) -> Result<(), EcpError> {
    let mut cmd = Command::new("curl");
    cmd.args([
        "-sSfL",
        "--retry",
        "2",
        "--max-time",
        "170",
        "-H",
        "User-Agent: ecp-update",
        "-o",
    ])
    .arg(dest)
    .arg(url);
    let out = safe_exec::output_with_timeout(cmd, DOWNLOAD_TIMEOUT).ok_or_else(|| {
        EcpError::Output(format!(
            "curl did not complete for {url} (missing, or timed out)"
        ))
    })?;
    if !out.status.success() {
        return Err(EcpError::Output(format!(
            "download failed: {url}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}

/// First token of a `<hex>  <file>` sidecar, lowercased. `None` unless it is a
/// 64-digit hex string, so a GitHub error page never becomes an expected digest.
pub(crate) fn parse_sha256_sidecar(body: &str) -> Option<String> {
    let token = body.split_whitespace().next()?;
    (token.len() == 64 && token.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| token.to_ascii_lowercase())
}

pub(crate) fn verify_sha256(path: &Path, expected: &str) -> Result<(), EcpError> {
    let bytes = std::fs::read(path)
        .map_err(|e| EcpError::Output(format!("read {}: {e}", path.display())))?;
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
    let out = safe_exec::output_with_timeout(cmd, TOOL_TIMEOUT)
        .ok_or_else(|| EcpError::Output("tar did not complete (missing, or timed out)".into()))?;
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

fn old_path(exe: &Path) -> PathBuf {
    let mut s: OsString = exe.as_os_str().to_owned();
    s.push(".old");
    PathBuf::from(s)
}

/// Swap `new_bin` into `exe`'s place. The previous file is moved aside first
/// and restored if the second rename fails, so a half-done update never leaves
/// the path empty.
pub(crate) fn replace_binary(exe: &Path, new_bin: &Path) -> Result<(), EcpError> {
    let old = old_path(exe);
    let _ = std::fs::remove_file(&old);
    std::fs::rename(exe, &old)
        .map_err(|e| EcpError::Output(format!("move aside {}: {e}", exe.display())))?;
    if let Err(e) = std::fs::rename(new_bin, exe) {
        let _ = std::fs::rename(&old, exe);
        return Err(EcpError::Output(format!("install {}: {e}", exe.display())));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(exe, std::fs::Permissions::from_mode(0o755));
    }
    // Fails on Windows while the old binary is still running; swept next time.
    let _ = std::fs::remove_file(&old);
    Ok(())
}

fn version_of(exe: &Path) -> Result<String, EcpError> {
    let mut cmd = Command::new(exe);
    cmd.arg("--version");
    let out = safe_exec::output_with_timeout(cmd, TOOL_TIMEOUT)
        .ok_or_else(|| EcpError::Output(format!("{} --version did not complete", exe.display())))?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
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
        // A leftover from an earlier interrupted update must not block the swap.
        std::fs::write(old_path(&exe), b"stale").unwrap();

        replace_binary(&exe, &new_bin).unwrap();

        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
        assert!(!new_bin.exists());
        assert!(!old_path(&exe).exists());
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

        assert!(replace_binary(&exe, &dir.path().join("absent")).is_err());

        assert_eq!(std::fs::read(&exe).unwrap(), b"old");
        assert!(!old_path(&exe).exists());
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
}
