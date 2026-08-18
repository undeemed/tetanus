//! Session log: append-only JSONL event journal with resume.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("corrupt journal line {0}")]
    Corrupt(usize),
}

pub struct Session {
    path: PathBuf,
}

impl Session {
    pub fn open(path: impl AsRef<Path>) -> Self {
        Self { path: path.as_ref().to_path_buf() }
    }
    pub fn append(&self, ev: &harness_core::Event) -> Result<(), SessionError> {
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&self.path)?;
        writeln!(f, "{}", serde_json::to_string(ev).expect("event serializes"))?;
        Ok(())
    }
    pub fn replay(&self) -> Result<Vec<harness_core::Event>, SessionError> {
        let f = match std::fs::File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut out = Vec::new();
        for (i, line) in std::io::BufReader::new(f).lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() { continue; }
            out.push(serde_json::from_str(&line).map_err(|_| SessionError::Corrupt(i + 1))?);
        }
        Ok(out)
    }
}
