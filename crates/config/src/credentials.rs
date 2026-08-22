//! Where a secret lives, and why it is not in the settings document.
//!
//! The settings document is the wrong place for a credential and always was.
//! It is read into [`crate::Config`], published by `config.dump`, quoted in
//! diagnostics and copied into bug reports; the redaction rule in
//! [`crate::secret`] exists precisely because values that should never have
//! been there are. This is the right place: a separate store the harness owns,
//! whose values never enter a layer, never reach a dump, and never reach a
//! journal.
//!
//! **A reference is public; a value is not.** A configuration surface names a
//! credential by its reference - `DEEPSEEK_API_KEY`, a POSIX identifier - and
//! may say whether it is set and where it came from. It may never say what it
//! is. That split is what lets a settings page be useful without being a leak.
//!
//! **The environment wins, and is visibly read-only.** A key supplied as
//! `DEEPSEEK_API_KEY=... tetanus`, by a CI secret or by a container's `-e` is
//! this run's explicit intent, and nothing inside the process can edit it. So
//! it takes precedence, and a write against a reference the environment
//! supplies is refused rather than accepted into a file that resolution would
//! then ignore - a write that appears to succeed while the old value keeps
//! being used is the worst of the three possible behaviours.
//!
//! **An empty value is an absent one, everywhere.** A blank never masquerades
//! as a configured secret: it resolves to nothing, it describes as
//! unconfigured, and storing one is refused in favour of [`Credentials::unset`].
//! Upstream states the same rule seam-wide, and the reason is the same defect
//! it prevents - a whitespace key that reads as present and goes to the
//! provider.
//!
//! **The file is owner-only, and checked before it is read.** The store writes
//! `0600`, but a hand-written or externally generated file carries whatever
//! umask produced it. Serving secrets out of a world-readable file would make
//! the mode this store promises meaningless, so a file other users can read is
//! refused rather than read.
//!
//! Parity: upstream `packages/credentials` and `credentials-local`, pinned by
//! their `credentials.spec.ts` and `local.spec.ts`. Upstream's four layers
//! collapse to two here: it has a `.env` fallback in the invocation directory
//! and another in its home, and tetanus reads the process environment and its
//! own store. Its hot-reload watcher and its cross-process write lock are a
//! surface this crate does not have; a value is re-read from the file on every
//! resolve instead, which gives the same "a changed credential reaches the
//! next operation" property without a watcher.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// The file a store keeps under the harness home.
pub const CREDENTIALS_FILE: &str = ".credentials.json";

/// What a secret prints as. The same word the boundary uses for a redacted
/// setting (`tetanus_protocol::types::REDACTED`), spelled here because this
/// crate is below the boundary and must not depend on it.
pub const REDACTED: &str = "<redacted>";

/// Permission bits outside the owner. A credentials file must have none.
#[cfg(unix)]
const GROUP_OTHER_BITS: u32 = 0o077;

#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    /// The reference is not a name this store addresses. Deliberately the
    /// POSIX identifier rule, because a reference doubles as an environment
    /// variable name and one that could not be exported would be a reference
    /// only half the layers could serve.
    #[error("credential reference {0:?} must match [A-Za-z_][A-Za-z0-9_]*")]
    BadReference(String),
    /// A blank is not a secret. Use [`Credentials::unset`].
    #[error("{0}: a credential cannot be empty; remove it instead")]
    EmptyValue(String),
    /// The environment supplies this reference, and nothing in the process can
    /// edit the environment it was launched with.
    #[error(
        "{0} is supplied by the environment, which this store cannot write: unset it in the \
         environment first, or the stored value would never be the one used"
    )]
    ShadowedByEnvironment(String),
    #[error("{}: is readable beyond its owner (mode {mode:o}); run `chmod 600` on it", path.display())]
    TooOpen { path: PathBuf, mode: u32 },
    #[error("{}: cannot be read: {source}", path.display())]
    Unreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{}: cannot be written: {source}", path.display())]
    Unwritable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The document did not parse. The parser's own message is deliberately
    /// not carried: it quotes the offending line, and in this file that line
    /// is a secret.
    #[error("{}: does not parse as a credentials document", path.display())]
    Malformed { path: PathBuf },
}

/// Which layer supplied a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CredentialSource {
    /// The process environment this run was launched with. Read-only.
    Environment,
    /// The store's own file. Writable.
    Store,
}

/// What a configuration surface may know about one reference: everything
/// except the value.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CredentialInfo {
    /// Whether [`Credentials::resolve`] would answer with a value now.
    pub configured: bool,
    /// The layer that would supply it; absent while unconfigured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<CredentialSource>,
    /// Whether [`Credentials::set`] would succeed for this reference now.
    pub writable: bool,
}

/// One resolved credential, and where it came from.
///
/// Deliberately not `Clone` and deliberately not `Debug`: a secret that can be
/// formatted is a secret that ends up in a log line the first time someone
/// debug-prints the struct that holds it. [`Credentials::resolve`] hands this
/// out, [`Secret::expose`] is the one way to read it, and the name of that
/// method is the point.
pub struct Secret {
    value: String,
    source: CredentialSource,
}

impl Secret {
    /// The value, for the one caller that has to put it on a wire.
    ///
    /// Named to be uncomfortable to write. Every call is a place a secret
    /// leaves the store, and there should be few enough of them to read in one
    /// sitting.
    pub fn expose(&self) -> &str {
        &self.value
    }

    pub fn source(&self) -> CredentialSource {
        self.source
    }
}

/// A `Secret` prints as its reference's redaction, never as itself, so a
/// format string cannot leak one by accident.
impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(REDACTED)
    }
}

impl std::fmt::Display for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(REDACTED)
    }
}

/// The credential store: the process environment over an owner-only file.
#[derive(Debug, Clone)]
pub struct Credentials {
    path: PathBuf,
}

impl Credentials {
    /// A store over the file at `path`.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// A store over `<home>/.credentials.json`.
    pub fn under(home: impl AsRef<Path>) -> Self {
        Self::at(home.as_ref().join(CREDENTIALS_FILE))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Resolve one reference to its current value, or `None` while it is
    /// unconfigured.
    ///
    /// Read per call, never cached. That is what makes a credential changed in
    /// the file reach the next operation without a restart, and it is also
    /// what keeps a stale secret from living in memory after it was revoked.
    pub fn resolve(&self, reference: &str) -> Result<Option<Secret>, CredentialError> {
        check_reference(reference)?;
        if let Some(value) = from_environment(reference) {
            return Ok(Some(Secret {
                value,
                source: CredentialSource::Environment,
            }));
        }
        Ok(self
            .read()?
            .remove(reference)
            .filter(|v| !v.is_empty())
            .map(|value| Secret {
                value,
                source: CredentialSource::Store,
            }))
    }

    /// Describe one reference without exposing it.
    pub fn describe(&self, reference: &str) -> Result<CredentialInfo, CredentialError> {
        check_reference(reference)?;
        let shadowed = from_environment(reference).is_some();
        let source = match (shadowed, self.stored(reference)?) {
            (true, _) => Some(CredentialSource::Environment),
            (false, true) => Some(CredentialSource::Store),
            (false, false) => None,
        };
        Ok(CredentialInfo {
            configured: source.is_some(),
            source,
            // Writable is about this reference, not about the file: a
            // reference the environment supplies cannot be usefully written
            // even when the file is perfectly writable.
            writable: !shadowed,
        })
    }

    /// Every reference this store holds a value for, in name order.
    ///
    /// The names, never the values. It is what a settings page lists.
    pub fn references(&self) -> Result<Vec<String>, CredentialError> {
        Ok(self.read()?.into_keys().collect())
    }

    /// Store one value.
    pub fn set(&self, reference: &str, value: &str) -> Result<(), CredentialError> {
        check_reference(reference)?;
        if value.trim().is_empty() {
            return Err(CredentialError::EmptyValue(reference.to_string()));
        }
        if from_environment(reference).is_some() {
            return Err(CredentialError::ShadowedByEnvironment(
                reference.to_string(),
            ));
        }
        let mut held = self.read()?;
        held.insert(reference.to_string(), value.to_string());
        self.write(&held)
    }

    /// Remove one value. Removing one that is not there is not an error.
    pub fn unset(&self, reference: &str) -> Result<bool, CredentialError> {
        check_reference(reference)?;
        if from_environment(reference).is_some() {
            return Err(CredentialError::ShadowedByEnvironment(
                reference.to_string(),
            ));
        }
        let mut held = self.read()?;
        let removed = held.remove(reference).is_some();
        if removed {
            self.write(&held)?;
        }
        Ok(removed)
    }

    fn stored(&self, reference: &str) -> Result<bool, CredentialError> {
        Ok(self
            .read()?
            .get(reference)
            .is_some_and(|value| !value.is_empty()))
    }

    /// The file's entries, or an empty store when there is no file.
    fn read(&self) -> Result<BTreeMap<String, String>, CredentialError> {
        self.check_mode()?;
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(BTreeMap::new())
            }
            Err(source) => {
                return Err(CredentialError::Unreadable {
                    path: self.path.clone(),
                    source,
                })
            }
        };
        if text.trim().is_empty() {
            return Ok(BTreeMap::new());
        }
        // The parse error is dropped on purpose: serde_json's message quotes
        // the offending input, and every line of this file is a secret.
        let held: BTreeMap<String, String> =
            serde_json::from_str(&text).map_err(|_| CredentialError::Malformed {
                path: self.path.clone(),
            })?;
        for reference in held.keys() {
            check_reference(reference)?;
        }
        Ok(held)
    }

    /// Replace the file, owner-only and atomically.
    ///
    /// The temporary is created with the final mode rather than chmodded
    /// afterwards: a file that is briefly world-readable is a file another
    /// user can read, and "briefly" is not a defence.
    fn write(&self, held: &BTreeMap<String, String>) -> Result<(), CredentialError> {
        let unwritable = |source| CredentialError::Unwritable {
            path: self.path.clone(),
            source,
        };
        if let Some(dir) = self.path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir).map_err(unwritable)?;
            }
        }
        let text = serde_json::to_string_pretty(held)
            .map_err(|error| unwritable(std::io::Error::other(error)))?;
        let temporary = self.path.with_extension("tmp");

        let written = (|| -> std::io::Result<()> {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&temporary, &self.path)?;
            Ok(())
        })();

        if let Err(source) = written {
            let _ = std::fs::remove_file(&temporary);
            return Err(unwritable(source));
        }
        Ok(())
    }

    /// Refuse a file other users can read, before its contents are read.
    ///
    /// POSIX only: Windows expresses this through ACLs, which are not the same
    /// thing and cannot be checked here, so the check is skipped rather than
    /// faked.
    fn check_mode(&self) -> Result<(), CredentialError> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let Ok(metadata) = std::fs::metadata(&self.path) else {
                return Ok(());
            };
            let mode = metadata.permissions().mode() & 0o777;
            if mode & GROUP_OTHER_BITS != 0 {
                return Err(CredentialError::TooOpen {
                    path: self.path.clone(),
                    mode,
                });
            }
        }
        Ok(())
    }
}

/// A reference's value in the process environment, if it holds a real one.
fn from_environment(reference: &str) -> Option<String> {
    std::env::var(reference)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

/// Whether a reference is a name this store addresses.
fn check_reference(reference: &str) -> Result<(), CredentialError> {
    let mut chars = reference.chars();
    let shaped = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    match shaped {
        true => Ok(()),
        false => Err(CredentialError::BadReference(reference.to_string())),
    }
}
