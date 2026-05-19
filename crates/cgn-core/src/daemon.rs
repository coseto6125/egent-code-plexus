//! Cross-platform process detachment helper.
//! Spec §4.5: spawn a child that survives the parent's exit, redirects
//! stdin/out/err to /dev/null, and runs in its own session.

use std::io;
use std::process::{Command, Stdio};

/// Spawn `args[0]` with `args[1..]` as a detached background process.
/// Parent returns immediately (no wait). Child is fully isolated.
pub fn spawn_detached(args: &[&str]) -> io::Result<()> {
    if args.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty argv"));
    }

    let mut cmd = Command::new(args[0]);
    cmd.args(&args[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            nix::unistd::setsid().map_err(|e| io::Error::other(format!("setsid: {e}")))?;
            Ok(())
        });
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x00000008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }

    cmd.spawn()?;
    Ok(())
}
