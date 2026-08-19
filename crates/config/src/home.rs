//! The harness home: the one root under which tetanus keeps user data.
//!
//! Parity: upstream `packages/util/home-paths` `resolveDshHome`.

use std::path::{Path, PathBuf};

/// The environment variable that moves the harness home.
pub const HOME_ENV: &str = "TETANUS_HOME";

/// The home's directory name under the operating-system home.
pub const HOME_DIR: &str = ".tetanus";

/// Resolve the harness home. Precedence, highest first: `configured`,
/// `$TETANUS_HOME`, then `~/.tetanus`.
pub fn home(configured: Option<&Path>) -> PathBuf {
    home_from(configured, std::env::var(HOME_ENV).ok().as_deref())
}

/// [`home`], reading the override from an argument rather than the process
/// environment, so a case can pin the rule without setting a global.
///
/// A blank or whitespace-only override counts as unset: an empty
/// `$TETANUS_HOME` must never resolve the home to the working directory, which
/// is where every one of the harness's files would then land.
pub fn home_from(configured: Option<&Path>, from_env: Option<&str>) -> PathBuf {
    if let Some(path) = configured {
        return expand(path);
    }
    match from_env {
        Some(value) if !value.trim().is_empty() => expand(Path::new(value.trim())),
        _ => os_home().join(HOME_DIR),
    }
}

/// Expand a leading `~` against the operating-system home. A `~` anywhere else
/// in the path is an ordinary character, as it is to a shell.
fn expand(path: &Path) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path.to_path_buf();
    };
    match text {
        "~" => os_home(),
        _ if text.starts_with("~/") || text.starts_with("~\\") => os_home().join(&text[2..]),
        _ => path.to_path_buf(),
    }
}

/// The operating-system home directory. Falls back to the working directory,
/// which is what a process with no home has.
fn os_home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
}
