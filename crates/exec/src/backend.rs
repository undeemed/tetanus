//! Which shell a command runs through, and how that shell is asked.
//!
//! Two backends ship: `bash`, which every POSIX deployment has, and `pwsh`,
//! which is what a Windows deployment has instead. They are behind one trait
//! for one reason: a seam with a single hard-coded `bash -c` designs Windows
//! out, and designing it back in later means changing every caller rather than
//! adding a backend.
//!
//! **A missing binary is refused, loudly.** A backend whose program is not on
//! this host answers [`BackendError::Missing`], naming the program and every
//! place it looked. It does not quietly run `sh` instead: a `bashism` that
//! silently ran under dash - `[[`, `set -o pipefail`, an array - fails later,
//! somewhere else, with a message about syntax, and the user is left debugging
//! their command instead of their deployment. Upstream resolves the same way
//! (`resolveExecutable` verifies, `resolvePwshPath` walks candidates) and this
//! keeps the refusal rather than the guess.
//!
//! Parity: upstream `packages/shell/bash-local`, `packages/shell/pwsh-local`
//! and its `resolve.ts`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Model-friendly environment overrides: no colour, no pager, no interactive
/// terminal features that would garble what a model reads. Upstream's
/// `ENV_OVERRIDES`, and the same set Codex hard-codes.
///
/// They are defaults, not policy: a caller that names one of these itself
/// wins, because the caller knows something this list does not.
pub const ENV_OVERRIDES: [(&str, &str); 4] = [
    ("NO_COLOR", "1"),
    ("TERM", "dumb"),
    ("PAGER", "cat"),
    ("GIT_PAGER", "cat"),
];

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    /// The shell this backend runs is not on this host. Named as a deployment
    /// problem, with what was looked for and where, because that is what the
    /// reader has to fix.
    #[error("the {backend} backend needs {program:?}, which is not on this host (looked in: {}); nothing was run, and no other shell was substituted", listed(.looked_in))]
    Missing {
        backend: &'static str,
        program: String,
        looked_in: Vec<PathBuf>,
    },
}

fn listed(paths: &[PathBuf]) -> String {
    if paths.is_empty() {
        return "nowhere - PATH is empty".to_string();
    }
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// One shell, resolved to a program this host can actually start.
///
/// A value of this type has been probed: the program was found, so a caller
/// holding one cannot be the caller that discovers the shell is missing
/// halfway through a turn.
#[derive(Debug, Clone)]
pub struct Resolved {
    backend: &'static str,
    program: PathBuf,
}

impl Resolved {
    /// The backend's name, as a deployment and a tool argument spell it.
    pub fn backend(&self) -> &'static str {
        self.backend
    }

    /// The program that will be started.
    pub fn program(&self) -> &Path {
        &self.program
    }
}

/// How one shell is asked to run something.
pub trait ShellBackend: Send + Sync {
    /// The name a deployment and a tool argument use.
    fn name(&self) -> &'static str;

    /// Find the shell on this host, or say why it is not here.
    fn resolve(&self) -> Result<Resolved, BackendError>;

    /// The arguments that run one command line and exit.
    ///
    /// The command is one argument, never concatenated into a longer line:
    /// the shell splits its own script, and nothing between this seam and the
    /// shell gets a second go at it.
    fn one_shot(&self, command: &str) -> Vec<String>;

    /// The arguments that start a shell reading commands from its input until
    /// the input closes - the long-lived session a turn reuses.
    fn session(&self) -> Vec<String>;

    /// What a fresh session has to be told before it behaves like one, one
    /// command per line.
    ///
    /// Bash puts its own stderr onto its stdout here, so a session transcript
    /// reads in the order the shell wrote it rather than in the order two
    /// pipes happened to be drained - which is what a terminal gives, and what
    /// a model reading a warning next to the line that provoked it needs.
    fn setup(&self) -> Vec<String> {
        Vec::new()
    }

    /// The environment this backend wants, before the caller's own entries.
    fn environment(&self) -> BTreeMap<String, String> {
        ENV_OVERRIDES
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    /// One command, wrapped so a persistent session says where its output
    /// begins, where it ends, and what the command exited with.
    ///
    /// It has to be one physical line: a session shell reading a script from
    /// its input treats a newline as the end of a command, so a wrapper split
    /// across lines would run its own halves as separate commands.
    fn wrap(&self, command: &str, markers: &Markers) -> String;
}

/// The two strings a persistent session watches for around one command.
///
/// They carry a nonce because a command can print anything, including
/// something that looks like a marker. A marker a command could guess is a
/// command that can lie about its own exit status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Markers {
    pub start: String,
    pub end: String,
}

impl Markers {
    /// Fresh markers, keyed by a nonce nothing else in this process shares.
    pub fn new(nonce: &str) -> Self {
        Self {
            start: format!("__tetanus_begin_{nonce}__"),
            end: format!("__tetanus_end_{nonce}__:"),
        }
    }
}

/// Bash: the POSIX default, and the one every ported case runs under.
#[derive(Debug, Clone, Default)]
pub struct Bash {
    /// An explicit path, trusted as given. A deployment that knows where its
    /// bash is does not need this seam's search, and upstream trusts a
    /// configured `pwshPath` the same way.
    program: Option<PathBuf>,
}

impl Bash {
    pub fn new() -> Self {
        Self::default()
    }

    /// A backend pinned to one program, without a search.
    pub fn at(program: impl Into<PathBuf>) -> Self {
        Self {
            program: Some(program.into()),
        }
    }
}

impl ShellBackend for Bash {
    fn name(&self) -> &'static str {
        "bash"
    }

    fn resolve(&self) -> Result<Resolved, BackendError> {
        resolve_program("bash", self.program.clone(), &[PathBuf::from("bash")])
    }

    fn one_shot(&self, command: &str) -> Vec<String> {
        vec!["-c".to_string(), command.to_string()]
    }

    fn session(&self) -> Vec<String> {
        // No profile and no rc file: a session whose behaviour depends on the
        // operator's dotfiles is a session that behaves differently for every
        // deployment, and a model cannot be told which one it got.
        vec!["--noprofile".to_string(), "--norc".to_string()]
    }

    fn setup(&self) -> Vec<String> {
        vec!["exec 2>&1".to_string()]
    }

    fn wrap(&self, command: &str, markers: &Markers) -> String {
        format!(
            "printf '%s\\n' {start}; eval -- {command}; __tetanus_status=$?; printf '%s%s\\n' {end} \"$__tetanus_status\"",
            start = quote(&markers.start),
            command = quote(command),
            end = quote(&markers.end),
        )
    }
}

/// PowerShell: what a Windows deployment runs instead, behind the same trait.
///
/// It is here so Windows is a backend that is absent on this host rather than
/// a platform this seam cannot express. On a host without it every call
/// answers [`BackendError::Missing`], which is the behaviour under test.
#[derive(Debug, Clone, Default)]
pub struct PowerShell {
    program: Option<PathBuf>,
}

/// Make the console speak UTF-8 before anything else runs, so what a model
/// reads is what the command printed. Upstream's `ENCODING_PREAMBLE`.
pub const PWSH_ENCODING_PREAMBLE: &str = "[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); $OutputEncoding = [System.Text.UTF8Encoding]::new($false); ";

impl PowerShell {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn at(program: impl Into<PathBuf>) -> Self {
        Self {
            program: Some(program.into()),
        }
    }

    /// Where a PowerShell may be, newest first: PowerShell 7 where its
    /// installer puts it, then whatever is on PATH (a Microsoft Store install
    /// lands there), then Windows PowerShell 5.1 as the last resort. Upstream
    /// walks the same list.
    fn candidates() -> Vec<PathBuf> {
        let program_files =
            std::env::var("ProgramFiles").unwrap_or_else(|_| "C:\\Program Files".to_string());
        let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
        vec![
            PathBuf::from(program_files)
                .join("PowerShell")
                .join("7")
                .join("pwsh.exe"),
            PathBuf::from("pwsh"),
            PathBuf::from("pwsh.exe"),
            PathBuf::from(system_root)
                .join("System32")
                .join("WindowsPowerShell")
                .join("v1.0")
                .join("powershell.exe"),
        ]
    }
}

impl ShellBackend for PowerShell {
    fn name(&self) -> &'static str {
        "pwsh"
    }

    fn resolve(&self) -> Result<Resolved, BackendError> {
        resolve_program("pwsh", self.program.clone(), &Self::candidates())
    }

    fn one_shot(&self, command: &str) -> Vec<String> {
        vec![
            "-NoLogo".to_string(),
            "-NoProfile".to_string(),
            "-NonInteractive".to_string(),
            "-Command".to_string(),
            format!("{PWSH_ENCODING_PREAMBLE}{command}"),
        ]
    }

    fn session(&self) -> Vec<String> {
        vec![
            "-NoLogo".to_string(),
            "-NoProfile".to_string(),
            "-NonInteractive".to_string(),
            "-Command".to_string(),
            "-".to_string(),
        ]
    }

    fn setup(&self) -> Vec<String> {
        // PowerShell has no `exec 2>&1`: a redirection applies to a command,
        // not to the shell, so its two pipes are drained separately and the
        // transcript keeps each line whole rather than each ordering exact.
        vec![PWSH_ENCODING_PREAMBLE.trim_end().to_string()]
    }

    fn wrap(&self, command: &str, markers: &Markers) -> String {
        // `$global:LASTEXITCODE` is only set by a native program, so a failing
        // cmdlet is read off `$?` instead; between them they say what a model
        // means by "did it work".
        format!(
            "Write-Output {start}; try {{ {command} }} catch {{ Write-Error $_ }}; $__tetanus_status = if ($LASTEXITCODE -ne $null) {{ $LASTEXITCODE }} elseif ($?) {{ 0 }} else {{ 1 }}; Write-Output \"{end_prefix}$__tetanus_status\"",
            start = pwsh_quote(&markers.start),
            command = command.replace('\n', "; "),
            end_prefix = markers.end,
        )
    }
}

/// Find `program` on this host: an explicit path is trusted, a bare name is
/// looked up on PATH, and an absolute candidate is probed where it stands.
fn resolve_program(
    backend: &'static str,
    explicit: Option<PathBuf>,
    candidates: &[PathBuf],
) -> Result<Resolved, BackendError> {
    if let Some(program) = explicit {
        // Trusted as given, and still probed: an operator who wrote a path
        // that is not there gets the same loud refusal as an absent default,
        // naming what they wrote.
        if runnable(&program) {
            return Ok(Resolved { backend, program });
        }
        return Err(BackendError::Missing {
            backend,
            program: program.display().to_string(),
            looked_in: vec![program],
        });
    }

    let mut looked_in = Vec::new();
    for candidate in candidates {
        if candidate.components().count() > 1 {
            looked_in.push(candidate.clone());
            if runnable(candidate) {
                return Ok(Resolved {
                    backend,
                    program: candidate.clone(),
                });
            }
            continue;
        }
        for directory in path_entries() {
            let full = directory.join(candidate);
            looked_in.push(full.clone());
            if runnable(&full) {
                return Ok(Resolved {
                    backend,
                    program: full,
                });
            }
        }
    }

    Err(BackendError::Missing {
        backend,
        program: candidates
            .first()
            .map(|first| first.display().to_string())
            .unwrap_or_else(|| backend.to_string()),
        looked_in,
    })
}

/// The directories PATH names, in order.
fn path_entries() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default()
}

/// Whether a path names something that can be started.
///
/// The question is asked of the entry itself and not of what it points at,
/// because a Windows Store execution alias is a reparse point whose target
/// this process cannot stat, and `CreateProcess` starts it anyway. A directory
/// never qualifies.
fn runnable(path: &Path) -> bool {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata.is_file() || metadata.is_symlink(),
        Err(_) => false,
    }
}

/// Quote one string as a bash `$'...'` literal, which survives a newline, a
/// quote and a backslash - the three things a model's command line will
/// contain the first time it writes one.
fn quote(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace('\r', "\\r")
        .replace('\n', "\\n");
    format!("$'{escaped}'")
}

/// Quote one string as a PowerShell single-quoted literal, where the only
/// escape is a doubled quote.
fn pwsh_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}
