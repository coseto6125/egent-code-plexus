//! Shared fixtures for MCP integration tests.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

fn stub_test_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

pub fn stub_guard() -> MutexGuard<'static, ()> {
    stub_test_lock().lock().unwrap()
}

/// Write an executable shell script (stub `ecp`) into `dir` and return its path.
pub fn write_stub(dir: &Path, script: &str) -> PathBuf {
    #[cfg(windows)]
    return write_cmd_stub(dir, script);

    #[cfg(unix)]
    {
        write_unix_stub(dir, script)
    }
}

#[cfg(unix)]
fn write_unix_stub(dir: &Path, script: &str) -> PathBuf {
    let stub = dir.join("ecp");
    std::fs::write(&stub, script).unwrap();
    use std::os::unix::fs::PermissionsExt;

    let mut perms = std::fs::metadata(&stub).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&stub, perms).unwrap();
    stub
}

#[cfg(windows)]
fn write_cmd_stub(dir: &Path, script: &str) -> PathBuf {
    let stub = dir.join("ecp.cmd");
    let body = if script.contains("ecp group find") {
        r#"@echo off
:loop
if "%~1"=="" goto ok
if "%~1"=="@all" (
  shift
  goto loop
)
set "arg=%~1"
if "%arg:~0,1%"=="@" (
  >&2 echo error: cannot be used at the top level - use `ecp group find` instead
  exit /b 1
)
shift
goto loop
:ok
echo ok
"#
    } else if script.contains("echo 'boom'") {
        "@echo off\r\necho boom 1>&2\r\nexit /b 1\r\n"
    } else if script.contains("sub=$1 arg1=$2 arg2=$3") {
        "@echo off\r\necho sub=%1 arg1=%2 arg2=%3\r\n"
    } else if script.contains("sub=$1 a1=$2 a2=$3 a3=$4") {
        "@echo off\r\necho sub=%1 a1=%2 a2=%3 a3=%4\r\n"
    } else if script.contains("echo \"$@\"") {
        "@echo off\r\necho %*\r\n"
    } else {
        "@echo off\r\necho ok\r\n"
    };
    std::fs::write(&stub, body).unwrap();
    stub
}
