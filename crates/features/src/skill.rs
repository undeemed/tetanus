//! Skills: instructions a project or a person keeps on disk, offered to the
//! model as things it can ask for by name.
//!
//! A skill is a Markdown file with a little frontmatter. It is the answer to
//! "how do we do X here" - deploy, cut a release, run the slow tests - written
//! once, by whoever knows, and read by the model at the moment it needs it
//! rather than pasted into every prompt.
//!
//! **Discovery is ordered, and the earlier root wins.** The project's own
//! skills come first, then the ones a deployment named, then the user's. A name
//! defined twice resolves to the first, and the loser is *recorded* as shadowed
//! rather than dropped - because "my skill does nothing" has no answer anywhere
//! if the roster cannot say what displaced it. `crates/config/src/preset.rs`
//! settled that rule for presets and this follows it exactly; two rosters in
//! one workspace disagreeing about precedence would be a trap.
//!
//! **A candidate that is not a working skill is reported, not skipped.** A file
//! with broken frontmatter, or no description, is listed as a fault with its
//! path. A skill that simply fails to appear gives its author nowhere to look.
//! One broken file never hides its valid siblings.
//!
//! **A root that cannot be read is a fault, not an empty root.** Absence and
//! refusal are different facts, and answering "no skills here" for a directory
//! this process was denied would serve a deployment a roster it did not choose.
//!
//! **Not every skill is the model's to invoke.** `disable-model-invocation` in
//! the frontmatter keeps a skill out of the catalogue the model reads, for the
//! ones a person runs deliberately. It is a property of the skill, so it
//! travels with the file rather than living in a deployment's configuration.
//!
//! Parity: upstream `packages/skill/skill` and `packages/skill/skill-filesystem`,
//! pinned by their `skill.spec.ts` and `skill-filesystem.spec.ts`. Upstream's
//! provider registry, its root watcher and its durable catalogue injection are
//! named in `docs/parity.md`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};
use tetanus_turn::tools::{Tool, ToolError, ToolMode, ToolOutcome, ToolSchema};

/// Where a skill was found, in precedence order.
///
/// The order of the variants is the precedence, and [`Source::rank`] is derived
/// from it, so adding a root in the middle cannot leave the two disagreeing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Source {
    /// `<project>/.tetanus/skills`: this repository's own.
    Project,
    /// `<project>/.agents/skills`: the convention shared with other tools.
    ProjectAgents,
    /// A directory the deployment named.
    Custom,
    /// `<harness home>/skills`: this user's.
    User,
    /// `~/.agents/skills`: this user's, in the shared convention.
    UserAgents,
}

impl Source {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::ProjectAgents => "project-agents",
            Self::Custom => "custom",
            Self::User => "user",
            Self::UserAgents => "user-agents",
        }
    }
}

/// One root to search, and what finding something there means.
#[derive(Debug, Clone)]
pub struct Root {
    pub path: PathBuf,
    pub source: Source,
}

impl Root {
    pub fn new(path: impl Into<PathBuf>, source: Source) -> Self {
        Self {
            path: path.into(),
            source,
        }
    }
}

/// The roots a deployment searches, in precedence order.
///
/// Built in one function so the order is stated once. A missing root is not a
/// problem here - most deployments have two of these five - and
/// [`discover`] treats "not there" and "cannot be read" differently.
pub fn default_roots(project: &Path, home: Option<&Path>, custom: &[PathBuf]) -> Vec<Root> {
    let mut roots = vec![
        Root::new(project.join(".tetanus/skills"), Source::Project),
        Root::new(project.join(".agents/skills"), Source::ProjectAgents),
    ];
    roots.extend(custom.iter().map(|path| Root::new(path, Source::Custom)));
    if let Some(home) = home {
        roots.push(Root::new(home.join("skills"), Source::User));
    }
    if let Some(agents) = user_agents_home() {
        roots.push(Root::new(agents.join("skills"), Source::UserAgents));
    }
    roots
}

/// `$TETANUS_AGENTS_HOME`, else `~/.agents`, else nothing.
///
/// Answering `None` rather than guessing a path is deliberate: a process with
/// no home directory is a container, and inventing `/root/.agents` for it would
/// produce a permission fault that reads as a bug.
fn user_agents_home() -> Option<PathBuf> {
    if let Some(named) = std::env::var_os("TETANUS_AGENTS_HOME") {
        return Some(PathBuf::from(named));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".agents"))
}

/// One skill, as its file defines it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    /// The name the model asks for. Taken from the frontmatter when it is
    /// there, and from the file or directory name when it is not, so a skill
    /// cannot be unnamed.
    pub name: String,
    /// One line saying what it is for. Required: an entry in a catalogue with
    /// no description is a name the model has to guess the meaning of.
    pub description: String,
    /// The instructions themselves - everything after the frontmatter.
    pub content: String,
    pub source: Source,
    pub path: PathBuf,
    /// Whether the model may invoke it. A skill a person runs deliberately sets
    /// `disable-model-invocation: true` and stays out of the catalogue.
    pub model_invocable: bool,
}

/// A candidate that is not a working skill, and why.
///
/// Reported rather than skipped: a skill that simply fails to appear gives its
/// author nowhere to look.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fault {
    pub path: PathBuf,
    pub reason: String,
}

/// A skill that lost a name to an earlier root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shadowed {
    pub name: String,
    /// The one that lost.
    pub path: PathBuf,
    pub source: Source,
    /// The one that won.
    pub by: PathBuf,
}

/// Everything one discovery pass found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Roster {
    /// By name, in name order - the order the model reads them in, and stable
    /// across runs so one prompt is byte-identical to the next.
    pub skills: BTreeMap<String, Skill>,
    pub shadowed: Vec<Shadowed>,
    pub faults: Vec<Fault>,
}

impl Roster {
    /// The skills the model may ask for, in name order.
    pub fn model_invocable(&self) -> Vec<&Skill> {
        self.skills
            .values()
            .filter(|skill| skill.model_invocable)
            .collect()
    }

    /// The catalogue line per skill, for a prompt section or a tool
    /// description.
    pub fn catalogue(&self) -> String {
        self.model_invocable()
            .into_iter()
            .map(|skill| format!("- {}: {}", skill.name, skill.description))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Search every root, in order, and answer what is there.
///
/// Never fails as a whole: a broken file is a fault, an unreadable root is a
/// fault, and everything that parsed is still returned. A discovery that
/// refused because one file was wrong would take every working skill down with
/// it.
pub fn discover(roots: &[Root]) -> Roster {
    let mut roster = Roster::default();
    for root in roots {
        match candidates(&root.path) {
            Ok(found) => {
                for (path, fallback) in found {
                    take(&mut roster, &path, &fallback, root.source);
                }
            }
            // Not being there is the ordinary case: most deployments have two
            // of the five roots.
            Err(None) => continue,
            Err(Some(reason)) => roster.faults.push(Fault {
                path: root.path.clone(),
                reason,
            }),
        }
    }
    roster
}

/// The skill files one root holds, in a stable order.
///
/// `Err(None)` is "there is no such root", which is ordinary; `Err(Some)` is a
/// root that exists and could not be read, which is a fault - answering "no
/// skills here" for a directory this process was denied would serve a roster
/// nobody chose.
fn candidates(root: &Path) -> Result<Vec<(PathBuf, String)>, Option<String>> {
    let entries = std::fs::read_dir(root).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => None,
        _ => Some(format!("the skill root could not be read: {error}")),
    })?;

    let mut found: Vec<(PathBuf, String)> = Vec::new();
    for entry in entries.flatten() {
        // A directory bundle is `<name>/SKILL.md`; a flat skill is `<name>.md`.
        // Both spellings exist in the wild, so reading only one would silently
        // ignore half of what a user has written.
        let path = entry.path();
        if path.is_dir() {
            let manifest = path.join("SKILL.md");
            if manifest.is_file() {
                found.push((manifest, file_stem(&path)));
            }
        } else if flat_skill(&path) {
            found.push((path.clone(), file_stem(&path)));
        }
    }
    // Sorted so one root contributes in a stable order whatever the
    // filesystem's own order is.
    found.sort();
    Ok(found)
}

/// Whether a file at the top of a root is a flat skill.
///
/// A bare `SKILL.md` is not: it is a bundle's manifest that has lost its
/// directory, and taking it would name the skill after the root.
fn flat_skill(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "md")
        && !path
            .file_name()
            .is_some_and(|name| name.eq_ignore_ascii_case("SKILL.md"))
}

/// Read one candidate into the roster: as a skill, as a shadowed loser, or as
/// a fault.
fn take(roster: &mut Roster, path: &Path, fallback: &str, source: Source) {
    match read_skill(path, fallback, source) {
        Ok(skill) => match roster.skills.get(&skill.name) {
            // First root wins, and the loser is recorded rather than dropped.
            Some(kept) => roster.shadowed.push(Shadowed {
                name: skill.name,
                path: skill.path,
                source: skill.source,
                by: kept.path.clone(),
            }),
            None => {
                roster.skills.insert(skill.name.clone(), skill);
            }
        },
        Err(reason) => roster.faults.push(Fault {
            path: path.to_path_buf(),
            reason,
        }),
    }
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// Read one skill file: its frontmatter, then its body.
pub fn read_skill(path: &Path, fallback_name: &str, source: Source) -> Result<Skill, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("it could not be read: {e}"))?;
    let (frontmatter, body) = split(&text);

    let declared = read_frontmatter(frontmatter)?;
    let name = declared
        .name
        .unwrap_or_else(|| fallback_name.trim().to_string());
    let description = declared.description;
    let model_invocable = declared.model_invocable;

    if name.is_empty() {
        return Err("it has no name, and its file name gave none either".into());
    }
    if description.is_empty() {
        return Err(
            "it has no `description`, so the model would have to guess what it is for".into(),
        );
    }
    Ok(Skill {
        name,
        description,
        content: body.trim().to_string(),
        source,
        path: path.to_path_buf(),
        model_invocable,
    })
}

/// What a skill file's frontmatter declared.
#[derive(Debug, Default)]
struct Declared {
    /// `None` when the file named none, so the caller's fallback stands.
    name: Option<String>,
    description: String,
    model_invocable: bool,
}

/// Read the keys this build knows out of a frontmatter block.
///
/// A key it does not know is ignored, so a file carrying metadata for another
/// tool still loads here. The exception is a *misspelling of a key this build
/// does know*: those are refused, because ignoring one fails open - the skill
/// stays in the model's catalogue, which is exactly what the key was written to
/// prevent.
fn read_frontmatter(pairs: Vec<(String, String)>) -> Result<Declared, String> {
    let mut declared = Declared {
        model_invocable: true,
        ..Declared::default()
    };
    for (key, value) in pairs {
        match key.as_str() {
            "name" => declared.name = Some(value.trim().to_string()).filter(|n| !n.is_empty()),
            "description" => declared.description = value.trim().to_string(),
            "disable-model-invocation" => {
                declared.model_invocable = !boolean(&value).ok_or_else(|| {
                    format!(
                        "`disable-model-invocation` must be true or false, not {value:?}. A \
                         spelling this build cannot read would silently offer a skill a person \
                         meant to keep back"
                    )
                })?;
            }
            "disableModelInvocation" | "model-invocable" | "modelInvocable" => {
                return Err(format!(
                    "{key:?} is not a key this build reads; write \
                     `disable-model-invocation: true`"
                ))
            }
            _ => {}
        }
    }
    Ok(declared)
}

/// The boolean spellings a frontmatter value may use.
///
/// A closed list rather than "anything truthy": a value outside it is a
/// mistake, and reading `disable-model-invocation: maybe` as false would offer
/// the model a skill somebody meant to keep back.
fn boolean(value: &str) -> Option<bool> {
    match value
        .trim()
        .trim_matches(['"', '\''].as_ref())
        .to_ascii_lowercase()
        .as_str()
    {
        "true" | "yes" | "on" => Some(true),
        "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Split a skill file into its frontmatter pairs and its body.
///
/// The parser is deliberately small: `key: value` lines between two `---`
/// fences, and nothing else. A full YAML parser would accept nested structures
/// this format has no meaning for, and the failure mode of accepting them is a
/// skill whose metadata silently does nothing. CRLF is handled because a file
/// written on Windows is a file, and a `---` inside a *value* does not end the
/// block - only a fence on a line of its own does.
fn split(text: &str) -> (Vec<(String, String)>, String) {
    let normalized = text.replace("\r\n", "\n");
    let Some(rest) = normalized.strip_prefix("---\n") else {
        return (Vec::new(), normalized);
    };
    let Some(end) = rest.find("\n---") else {
        // An unterminated fence is not frontmatter; the whole file is content.
        // Refusing here would make a file that merely starts with a horizontal
        // rule unreadable.
        return (Vec::new(), normalized);
    };
    let (block, body) = rest.split_at(end);
    let body = body
        .trim_start_matches('\n')
        .strip_prefix("---")
        .unwrap_or(body)
        .to_string();

    let pairs = block
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            let key = key.trim();
            if key.is_empty() || key.starts_with('#') {
                return None;
            }
            Some((
                key.to_string(),
                value.trim().trim_matches(['"', '\''].as_ref()).to_string(),
            ))
        })
        .collect();
    (pairs, body)
}

/// The tool that hands one skill's instructions to the model.
pub struct SkillTool {
    roster: Arc<Roster>,
}

impl SkillTool {
    pub const NAME: &'static str = "skill";

    /// Compose the tool over one discovery pass.
    ///
    /// The roster is settled here rather than re-read per call: what the model
    /// was offered and what it can invoke must be the same list, and a
    /// re-reading tool could accept a name that was not in the catalogue it
    /// advertised - or refuse one that was.
    pub fn new(roster: Arc<Roster>) -> Arc<Self> {
        Arc::new(Self { roster })
    }
}

#[async_trait::async_trait]
impl Tool for SkillTool {
    fn schema(&self) -> ToolSchema {
        let catalogue = self.roster.catalogue();
        ToolSchema {
            name: Self::NAME.into(),
            description: if catalogue.is_empty() {
                "Load a named skill: instructions this project or user keeps for a particular \
                 task. No skills are available in this workspace."
                    .to_string()
            } else {
                format!(
                    "Load a named skill: instructions this project or user keeps for a \
                     particular task. Available:\n{catalogue}"
                )
            },
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "enum": self
                            .roster
                            .model_invocable()
                            .into_iter()
                            .map(|skill| skill.name.clone())
                            .collect::<Vec<_>>(),
                        "description": "The skill to load.",
                    },
                },
                "required": ["name"],
            }),
        }
    }

    /// Loading a skill reads a file and changes nothing.
    fn mode(&self, _arguments: &Value) -> ToolMode {
        ToolMode::Parallel
    }

    async fn execute(&self, arguments: &Value) -> Result<ToolOutcome, ToolError> {
        let name = arguments
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        let Some(skill) = self.roster.skills.get(name) else {
            let available = self.roster.catalogue();
            return Ok(ToolOutcome::failed(if available.is_empty() {
                format!("There is no skill named {name:?}, and this workspace defines none.")
            } else {
                format!("There is no skill named {name:?}. Available:\n{available}")
            }));
        };
        // A skill kept back from the model is kept back at the call too, not
        // only at the catalogue: a model that guessed the name of a
        // user-invocable skill must not get it.
        if !skill.model_invocable {
            return Ok(ToolOutcome::failed(format!(
                "The skill {name:?} is not one you may invoke: it is run deliberately by a \
                 person."
            )));
        }
        Ok(ToolOutcome::ok(format!(
            "# Skill: {}\n\n{}\n",
            skill.name, skill.content
        )))
    }
}
