//! Running a deployment's hooks through the one process seam.
//!
//! `crates/hooks` owns the protocol - which hook fires on which event, what
//! goes on its stdin, what its exit code means - and deliberately owns no
//! process: its `HookExecutor` is a narrow seam, and its own module note says
//! the real executor belongs to the crate that runs commands. This is that
//! executor, so a configured hook is a real child with the guarantees every
//! other child here has: an argv nothing re-splits, an environment somebody
//! listed, a timeout that kills the whole process group, and captured output
//! bounded to something a turn can carry.
//!
//! **A hook is a deployment's program, not a model's.** That is the difference
//! that shapes the defaults. The command comes from a settings file an
//! operator wrote, so it runs under the executor's ordinary policy rather than
//! being confined tighter than the harness itself - and if a deployment wants
//! it confined, the policy is where it says so, once, for commands and hooks
//! together.
//!
//! **The environment is a list, not a scrub.** Upstream hands a hook
//! `process.env` minus a denylist. Nothing is inherited here, so the question
//! is inverted: [`HookEnv`] names what passes. It defaults to the variables a
//! shell script cannot work without - `PATH` above all, because a hook that
//! cannot find `jq` is a hook that fails for a reason nobody will guess - and
//! a deployment that wants more says which more.
//!
//! **A hook that cannot run at all is still not an error here.** The seam's
//! contract is that `Err` means infrastructure: no shell, an unusable working
//! directory. `crates/hooks` turns that into a non-blocking outcome with the
//! fault on stderr, because a broken hook must not take a turn down with it -
//! so the honest thing for this layer to do is report precisely which of the
//! two happened and let the protocol decide.
//!
//! Parity: upstream `packages/hooks/hook-protocol`'s runner over its
//! `ShellExecutor`, which its own suite duck-types and its bridges supply.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tetanus_hooks::runner::{HookExecResult, HookExecSpec, HookExecutor};

use crate::backend::ShellBackend;
use crate::shell::{ShellConfig, ShellError, ShellExec, ShellRequest};

/// Which of this process's environment a hook may see, and what to add.
///
/// The passed list is names rather than values because the point is to be
/// readable in a settings document: an operator can see that `PATH` reaches a
/// hook and `AWS_SECRET_ACCESS_KEY` does not.
#[derive(Debug, Clone)]
pub struct HookEnv {
    /// Names taken from this process's environment where they are set.
    pub passed: Vec<String>,
    /// Entries given outright, which beat anything passed.
    pub added: BTreeMap<String, String>,
}

impl Default for HookEnv {
    fn default() -> Self {
        Self {
            // What a shell script needs before it can do anything at all. Not
            // a guess: without `PATH` a hook cannot run a single program, and
            // without `HOME` the tools that keep a config under it behave as
            // though the machine were new.
            passed: ["PATH", "HOME", "LANG", "LC_ALL", "TZ", "TERM"]
                .into_iter()
                .map(str::to_string)
                .collect(),
            added: BTreeMap::new(),
        }
    }
}

impl HookEnv {
    /// Nothing at all: the strictest reading, for a deployment whose hooks are
    /// absolute paths that need no environment.
    pub fn empty() -> Self {
        Self {
            passed: Vec::new(),
            added: BTreeMap::new(),
        }
    }

    /// The environment one hook run starts from.
    fn resolved(&self) -> BTreeMap<String, String> {
        let mut env: BTreeMap<String, String> = self
            .passed
            .iter()
            .filter_map(|name| std::env::var(name).ok().map(|value| (name.clone(), value)))
            .collect();
        env.extend(self.added.clone());
        env
    }
}

/// How hooks are run.
#[derive(Debug, Clone)]
pub struct HookExecConfig {
    /// The shell configuration hooks run under. Its `max_timeout` is the
    /// ceiling a hook's own configured timeout is clamped to, so a deployment
    /// still has the last word on how long a hook may hold a turn.
    pub shell: ShellConfig,
    pub env: HookEnv,
}

impl Default for HookExecConfig {
    fn default() -> Self {
        Self {
            shell: ShellConfig {
                // The hook protocol's own default, which is also its longest
                // sensible run: ten minutes. Left as the ceiling rather than
                // the shell tool's, because a hook and a model's command are
                // different kinds of thing and the shorter cap would silently
                // shorten a hook a deployment configured.
                timeout: Duration::from_millis(tetanus_hooks::runner::DEFAULT_HOOK_TIMEOUT_MS),
                max_timeout: Duration::from_millis(tetanus_hooks::runner::DEFAULT_HOOK_TIMEOUT_MS),
                ..ShellConfig::default()
            },
            env: HookEnv::default(),
        }
    }
}

/// The executor `crates/hooks` asks for, over this crate's shell seam.
pub struct ShellHookExecutor {
    exec: ShellExec,
    env: HookEnv,
}

impl std::fmt::Debug for ShellHookExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellHookExecutor")
            .field("backend", &self.exec.backend().name())
            .field("timeout", &self.exec.config().max_timeout)
            .finish()
    }
}

impl ShellHookExecutor {
    /// Resolve the backend now, so a deployment whose shell is missing learns
    /// it while somebody is watching rather than the first time an event
    /// fires.
    pub fn new(
        backend: Arc<dyn ShellBackend>,
        config: HookExecConfig,
    ) -> Result<Arc<Self>, ShellError> {
        Ok(Arc::new(Self {
            exec: ShellExec::new(backend, config.shell)?,
            env: config.env,
        }))
    }

    /// Run one hook, or say why nothing ran.
    async fn execute(&self, spec: HookExecSpec) -> Result<HookExecResult, String> {
        let mut request = ShellRequest::new(&spec.command).stdin(spec.stdin);
        // Milliseconds on the seam, because that is what the hook protocol
        // speaks; the clamp to the deployment's ceiling happens in `resolve`.
        if spec.timeout_ms > 0 {
            request = request.timeout(Duration::from_millis(spec.timeout_ms));
        }
        if let Some(workdir) = spec.workdir.filter(|dir| !dir.trim().is_empty()) {
            request = request.workdir(workdir);
        }
        for (key, value) in self.env.resolved() {
            request = request.env(key, value);
        }
        for (key, value) in spec.env.unwrap_or_default() {
            request = request.env(key, value);
        }

        let resolved = self.exec.resolve(request).map_err(|refused| {
            format!("this hook could not be prepared and nothing ran: {refused}")
        })?;
        let run = self
            .exec
            .run(&resolved)
            .await
            .map_err(|refused| format!("this hook could not be run: {refused}"))?;

        // A hook that outran its budget is reported the way a hook killed by a
        // signal is: no exit code, and the reason on stderr. The protocol reads
        // "no code" as non-blocking, which is the right answer - a hook nobody
        // waited for has not decided anything.
        let mut stderr = run.output.stderr.text.clone();
        if run.output.timed_out() {
            let timeout = run.timeout.as_millis();
            if !stderr.is_empty() && !stderr.ends_with('\n') {
                stderr.push('\n');
            }
            stderr.push_str(&format!(
                "[the hook was killed after {timeout}ms, with everything it started]"
            ));
        }
        Ok(HookExecResult {
            exit_code: run.output.code,
            stdout: run.output.stdout.text.clone(),
            stderr,
        })
    }
}

impl HookExecutor for ShellHookExecutor {
    fn run<'a>(
        &'a self,
        spec: HookExecSpec,
    ) -> Pin<Box<dyn Future<Output = Result<HookExecResult, String>> + Send + 'a>> {
        Box::pin(self.execute(spec))
    }
}
