//! Best-effort detection of how this `ecp` binary was installed, so the version
//! check can suggest an upgrade command that matches the install channel rather
//! than always pointing at `cargo install`.
//!
//! The only signal available at runtime is `std::env::current_exe()`: each
//! channel lands the binary under a recognizable path. Detection is a
//! substring match normalized for case and path separator, so the same rules
//! work on Linux, macOS, and Windows. It is a heuristic — a binary that was
//! moved by hand, or symlinked onto PATH, may not match; the fallback lists
//! every channel so the user is never stranded.

/// How the running binary was most likely installed.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum InstallSource {
    /// npm platform package (`npx` / `npm i -g`): binary under `@egent-code-plexus/`.
    Npm,
    /// uv tool / `uvx`: binary under a `uv/tools` tree.
    UvTool,
    /// pip-installed wheel: binary in `site-packages/ecp/_bin`.
    Pip,
    /// `cargo install`: binary under a `.cargo/bin` tree.
    Cargo,
    /// Homebrew formula.
    Homebrew,
    /// Unknown channel (hand-moved binary, install.sh/.ps1 to a custom dir, …).
    Unknown,
}

impl InstallSource {
    /// Detect from the current executable's path. Defaults to `Unknown` when the
    /// path can't be read or matches no known channel.
    pub(crate) fn detect() -> Self {
        match std::env::current_exe() {
            Ok(p) => Self::from_exe_path(&p.to_string_lossy()),
            Err(_) => Self::Unknown,
        }
    }

    /// Classify an executable path. Pure over the string so it is unit-testable
    /// with paths from any platform.
    fn from_exe_path(raw: &str) -> Self {
        // Normalize: lowercase (Windows paths are case-insensitive) and unify
        // separators so a single `/`-based needle matches `\` paths too.
        let path = raw.to_lowercase().replace('\\', "/");
        if path.contains("@egent-code-plexus") {
            Self::Npm
        } else if path.contains("uv/tools") {
            Self::UvTool
        } else if path.contains("site-packages/ecp/_bin") {
            Self::Pip
        } else if path.contains(".cargo/bin") {
            Self::Cargo
        } else if path.contains("/homebrew/") || path.contains("/cellar/") {
            Self::Homebrew
        } else {
            Self::Unknown
        }
    }

    /// Upgrade command to surface in the version warning. `Unknown` lists every
    /// channel so the user can pick the one matching their install.
    pub(crate) fn upgrade_hint(self) -> &'static str {
        match self {
            Self::Npm => "npm install -g egent-code-plexus@latest  (or: npx egent-code-plexus@latest)",
            Self::UvTool => "uv tool upgrade egent-code-plexus  (or: uvx egent-code-plexus@latest)",
            Self::Pip => "pip install -U egent-code-plexus",
            Self::Cargo => {
                "cargo install --git https://github.com/coseto6125/egent-code-plexus egent-code-plexus --bin ecp --locked"
            }
            Self::Homebrew => "brew upgrade egent-code-plexus",
            Self::Unknown => {
                "upgrade via your install channel — npm: npm i -g egent-code-plexus@latest | \
                 uv: uv tool upgrade egent-code-plexus | pip: pip install -U egent-code-plexus | \
                 brew: brew upgrade egent-code-plexus | \
                 cargo: cargo install --git https://github.com/coseto6125/egent-code-plexus egent-code-plexus --bin ecp --locked"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_npm_across_separators() {
        // npm dispatches to the platform package's binary.
        assert_eq!(
            InstallSource::from_exe_path(
                "/home/u/proj/node_modules/@egent-code-plexus/linux-x64/bin/ecp"
            ),
            InstallSource::Npm
        );
        // Windows path + .exe + global npm cache.
        assert_eq!(
            InstallSource::from_exe_path(
                "C:\\Users\\U\\AppData\\Roaming\\npm\\node_modules\\@egent-code-plexus\\win32-x64\\bin\\ecp.exe"
            ),
            InstallSource::Npm
        );
    }

    #[test]
    fn detects_uv_tool() {
        assert_eq!(
            InstallSource::from_exe_path("/home/u/.local/share/uv/tools/egent-code-plexus/bin/ecp"),
            InstallSource::UvTool
        );
        assert_eq!(
            InstallSource::from_exe_path(
                "C:\\Users\\U\\AppData\\Local\\uv\\tools\\egent-code-plexus\\Scripts\\ecp.exe"
            ),
            InstallSource::UvTool
        );
    }

    #[test]
    fn detects_pip_wheel() {
        assert_eq!(
            InstallSource::from_exe_path("/home/u/.venv/lib/python3.12/site-packages/ecp/_bin/ecp"),
            InstallSource::Pip
        );
    }

    #[test]
    fn detects_cargo() {
        assert_eq!(
            InstallSource::from_exe_path("/home/u/.cargo/bin/ecp"),
            InstallSource::Cargo
        );
    }

    #[test]
    fn detects_homebrew() {
        assert_eq!(
            InstallSource::from_exe_path("/opt/homebrew/bin/ecp"),
            InstallSource::Homebrew
        );
        assert_eq!(
            InstallSource::from_exe_path("/usr/local/Cellar/egent-code-plexus/0.5.1/bin/ecp"),
            InstallSource::Homebrew
        );
    }

    #[test]
    fn unknown_for_hand_placed_binary() {
        // install.sh default (~/.local/bin) or a hand-moved binary — no channel
        // marker, so fall back to the multi-channel hint.
        assert_eq!(
            InstallSource::from_exe_path("/home/u/.local/bin/ecp"),
            InstallSource::Unknown
        );
        assert_eq!(
            InstallSource::from_exe_path("/usr/local/bin/ecp"),
            InstallSource::Unknown
        );
    }
}
