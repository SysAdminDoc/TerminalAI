//! Launch templates a repository declares about itself.
//!
//! A preset is the operator's, saved once and reused everywhere. A template is
//! the *repository's*: "this is how you start an agent on this project", written
//! by whoever knows the project and versioned alongside it, so a new session in
//! a familiar repo does not start by remembering which permission mode and
//! which extra directories that repo needs.
//!
//! The reason this is not just a preset with a different name is trust. A
//! preset comes from the person at the keyboard. A template comes from a file
//! in a repository, which may have been written by someone else, may have
//! arrived in a pull request, and is read before the operator has looked at it.
//! So the fields a template may set are an allowlist of *choices*, never
//! anything that reaches a command line directly:
//!
//! - `extra_args` is refused outright. It is documented in [`crate::launch`] as
//!   a trusted-input-only escape hatch, and a repository is not trusted input.
//!   A template that could set it would be arbitrary argument injection from a
//!   file that arrives with a clone.
//! - `cwd` is refused. The working directory is the repository the template was
//!   read from; letting the file redirect it would point the agent somewhere the
//!   operator did not choose.
//! - Every enumerated field is parsed into its own type, so an unknown value is
//!   a refusal rather than a string passed through to the CLI.
//!
//! A malformed file is an error, never an empty template list. Silently ignoring
//! a template the repository author wrote is how an operator ends up launching
//! with the wrong permission mode while believing the repository's own defaults
//! were applied.

use std::path::{Path, PathBuf};

use crate::agent::Agent;
use crate::launch::{Effort, LaunchSpec, Permission, Sandbox};

/// Where a repository declares its templates, relative to the repository root.
pub const TEMPLATE_FILE: &str = ".terminalai/templates.toml";

/// Cap on templates read from one file. A repository offering more than this is
/// a menu nobody reads, and the list is rendered in a launcher dropdown.
pub const MAX_TEMPLATES: usize = 32;
/// Cap on one template's prompt. Generous — the operator's own drain prompts run
/// to several kilobytes — and bounded so a file cannot dictate memory.
pub const MAX_PROMPT_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TemplateError {
    #[error("{TEMPLATE_FILE} is not valid TOML: {0}")]
    Parse(String),
    #[error("{TEMPLATE_FILE} could not be read: {0}")]
    Read(String),
    #[error("{TEMPLATE_FILE} declares an unusable value: {0}")]
    Invalid(String),
}

/// One launch configuration a repository offers.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Template {
    /// What the launcher shows. Required: an unnamed template cannot be chosen.
    pub name: String,
    /// Free text shown beside the name, for a template whose purpose is not
    /// obvious from it.
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub permission: Option<String>,
    #[serde(default)]
    pub sandbox: Option<String>,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    /// Extra writable directories, relative to the repository root.
    #[serde(default)]
    pub add_dirs: Vec<String>,
    #[serde(default)]
    pub worktree: bool,
    #[serde(default)]
    pub web_search: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TemplateFile {
    #[serde(default)]
    template: Vec<Template>,
}

/// Read the templates a repository declares, if it declares any.
///
/// A missing file is `Ok(Vec::new())` — most repositories will never have one.
pub fn load(repo_root: &Path) -> Result<Vec<Template>, TemplateError> {
    let path = repo_root.join(TEMPLATE_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(TemplateError::Read(error.to_string())),
    };
    parse(&text)
}

/// Parse and validate. Validation lives here rather than in [`load`] because
/// this is the innermost public entry point, and an invariant enforced only by
/// the outer one is not enforced for any caller that reaches this.
pub fn parse(text: &str) -> Result<Vec<Template>, TemplateError> {
    let file: TemplateFile =
        toml_edit::de::from_str(text).map_err(|error| TemplateError::Parse(error.to_string()))?;
    if file.template.len() > MAX_TEMPLATES {
        return Err(TemplateError::Invalid(format!(
            "{} templates exceeds the limit of {MAX_TEMPLATES}",
            file.template.len()
        )));
    }
    let mut seen = std::collections::BTreeSet::new();
    for template in &file.template {
        template.validate()?;
        if !seen.insert(template.name.trim().to_owned()) {
            // Two templates with one name means the launcher shows a choice
            // that does not identify what it launches.
            return Err(TemplateError::Invalid(format!(
                "two templates are both named {:?}",
                template.name.trim()
            )));
        }
    }
    Ok(file.template)
}

impl Template {
    fn validate(&self) -> Result<(), TemplateError> {
        if self.name.trim().is_empty() {
            return Err(TemplateError::Invalid(
                "a template needs a name to be chosen by".to_owned(),
            ));
        }
        if let Some(prompt) = &self.prompt {
            if prompt.len() > MAX_PROMPT_BYTES {
                return Err(TemplateError::Invalid(format!(
                    "the prompt in {:?} is {} bytes, over the {MAX_PROMPT_BYTES}-byte limit",
                    self.name.trim(),
                    prompt.len()
                )));
            }
        }
        // Each of these is parsed rather than passed through, so a value the
        // launcher does not model is refused here instead of reaching a CLI.
        if let Some(agent) = &self.agent {
            self.agent_choice(agent)?;
        }
        if let Some(effort) = &self.effort {
            parse_effort(effort).ok_or_else(|| self.unusable("effort", effort))?;
        }
        if let Some(permission) = &self.permission {
            parse_permission(permission).ok_or_else(|| self.unusable("permission", permission))?;
        }
        if let Some(sandbox) = &self.sandbox {
            parse_sandbox(sandbox).ok_or_else(|| self.unusable("sandbox", sandbox))?;
        }
        for dir in &self.add_dirs {
            // The directory is joined onto the repository root, so a parent
            // reference or an absolute path would grant the agent write access
            // somewhere the operator never chose.
            let path = Path::new(dir);
            if path.is_absolute()
                || path
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir))
            {
                return Err(TemplateError::Invalid(format!(
                    "{:?} adds {dir:?}, which is outside the repository",
                    self.name.trim()
                )));
            }
        }
        Ok(())
    }

    fn agent_choice(&self, agent: &str) -> Result<Agent, TemplateError> {
        match agent.trim().to_ascii_lowercase().as_str() {
            "claude" => Ok(Agent::Claude),
            "codex" => Ok(Agent::Codex),
            other => Err(self.unusable("agent", other)),
        }
    }

    fn unusable(&self, field: &str, value: &str) -> TemplateError {
        TemplateError::Invalid(format!(
            "{:?} sets {field} to {value:?}, which is not a value the launcher offers",
            self.name.trim()
        ))
    }

    /// Apply this template to a spec whose `cwd` is already the repository.
    ///
    /// The working directory is never taken from the template: it is the
    /// repository the template was read from, which is the one thing the
    /// operator did choose.
    pub fn apply(&self, repo_root: &Path, spec: &mut LaunchSpec) {
        if let Some(agent) = self.agent.as_ref().and_then(|a| self.agent_choice(a).ok()) {
            spec.agent = agent;
        }
        if let Some(model) = &self.model {
            spec.model = Some(model.clone());
        }
        if let Some(effort) = self.effort.as_deref().and_then(parse_effort) {
            spec.effort = Some(effort);
        }
        if let Some(permission) = self.permission.as_deref().and_then(parse_permission) {
            spec.permission = Some(permission);
        }
        if let Some(sandbox) = self.sandbox.as_deref().and_then(parse_sandbox) {
            spec.sandbox = Some(sandbox);
        }
        if let Some(profile) = &self.profile {
            spec.profile = Some(profile.clone());
        }
        if let Some(prompt) = &self.prompt {
            spec.initial_prompt = Some(prompt.clone());
        }
        spec.worktree = self.worktree;
        spec.web_search = self.web_search;
        spec.cwd = repo_root.to_path_buf();
        spec.add_dirs = self
            .add_dirs
            .iter()
            .map(|dir| repo_root.join(dir))
            .collect::<Vec<PathBuf>>();
    }
}

fn parse_effort(value: &str) -> Option<Effort> {
    match value.trim().to_ascii_lowercase().as_str() {
        "low" => Some(Effort::Low),
        "medium" => Some(Effort::Medium),
        "high" => Some(Effort::High),
        "xhigh" => Some(Effort::XHigh),
        "max" => Some(Effort::Max),
        _ => None,
    }
}

fn parse_permission(value: &str) -> Option<Permission> {
    match value.trim().to_ascii_lowercase().as_str() {
        "ask" => Some(Permission::Ask),
        "plan" => Some(Permission::Plan),
        "accept-edits" => Some(Permission::AcceptEdits),
        "bypass" => Some(Permission::Bypass),
        _ => None,
    }
}

fn parse_sandbox(value: &str) -> Option<Sandbox> {
    match value.trim().to_ascii_lowercase().as_str() {
        "read-only" => Some(Sandbox::ReadOnly),
        "workspace-write" => Some(Sandbox::WorkspaceWrite),
        "danger-full-access" => Some(Sandbox::DangerFullAccess),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repository_can_declare_how_to_start_work_on_it() {
        let templates = parse(
            r#"
[[template]]
name = "Drain the roadmap"
description = "Work the tracker top to bottom"
agent = "claude"
effort = "high"
permission = "accept-edits"
worktree = true
prompt = "Read ROADMAP.md and implement the next item."
add_dirs = ["docs"]
"#,
        )
        .expect("templates");
        assert_eq!(templates.len(), 1);
        let mut spec = LaunchSpec {
            cwd: PathBuf::from(r"C:\repos\shop"),
            ..LaunchSpec::default()
        };
        templates[0].apply(Path::new(r"C:\repos\shop"), &mut spec);
        assert_eq!(spec.agent, Agent::Claude);
        assert_eq!(spec.effort, Some(Effort::High));
        assert_eq!(spec.permission, Some(Permission::AcceptEdits));
        assert!(spec.worktree);
        assert_eq!(spec.add_dirs, vec![PathBuf::from(r"C:\repos\shop\docs")]);
    }

    #[test]
    fn a_template_cannot_set_raw_arguments() {
        // `extra_args` is documented as trusted-input-only, and a file that
        // arrives with a clone is not trusted input. Rejected at the schema
        // rather than filtered afterwards, so it cannot be reintroduced by
        // someone adding a field.
        let error = parse("[[template]]\nname = \"x\"\nextra_args = [\"--dangerously-skip\"]\n")
            .expect_err("must refuse");
        assert!(matches!(error, TemplateError::Parse(_)), "{error:?}");
    }

    #[test]
    fn a_template_cannot_redirect_the_working_directory() {
        let error =
            parse("[[template]]\nname = \"x\"\ncwd = \"C:/somewhere/else\"\n").expect_err("refuse");
        assert!(matches!(error, TemplateError::Parse(_)), "{error:?}");
    }

    #[test]
    fn extra_directories_cannot_escape_the_repository() {
        // These become writable roots for the agent.
        for hostile in ["../../secrets", r"C:\Windows", "docs/../../.."] {
            let text = format!("[[template]]\nname = \"x\"\nadd_dirs = [\"{}\"]\n", hostile.replace('\\', "\\\\"));
            let error = parse(&text).expect_err(hostile);
            assert!(matches!(error, TemplateError::Invalid(_)), "{hostile}: {error:?}");
        }
    }

    #[test]
    fn an_unknown_enumerated_value_is_refused_rather_than_passed_through() {
        // Passing it through would put an unmodelled string on an agent's
        // command line.
        for (field, value) in [
            ("agent", "gemini"),
            ("effort", "extreme"),
            ("permission", "yolo"),
            ("sandbox", "none"),
        ] {
            let error = parse(&format!("[[template]]\nname = \"x\"\n{field} = \"{value}\"\n"))
                .expect_err(field);
            assert!(matches!(error, TemplateError::Invalid(_)), "{field}: {error:?}");
            assert!(error.to_string().contains(value), "{error}");
        }
    }

    #[test]
    fn an_unnamed_template_is_refused() {
        let error = parse("[[template]]\nname = \"  \"\n").expect_err("refuse");
        assert!(matches!(error, TemplateError::Invalid(_)), "{error:?}");
    }

    #[test]
    fn two_templates_with_one_name_are_refused() {
        // The launcher would otherwise show a choice that does not identify
        // what it launches.
        let error = parse("[[template]]\nname = \"a\"\n\n[[template]]\nname = \"a\"\n")
            .expect_err("refuse");
        assert!(error.to_string().contains("both named"), "{error}");
    }

    #[test]
    fn a_malformed_file_is_an_error_not_an_empty_list() {
        // Silently ignoring it launches with the operator believing the
        // repository's defaults were applied.
        assert!(parse("[[template]\nname = ").is_err());
    }

    #[test]
    fn a_repository_with_no_template_file_simply_has_none() {
        let dir = std::env::temp_dir().join(format!("terminalai-template-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        assert_eq!(load(&dir).expect("load"), Vec::new());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_oversized_prompt_is_refused() {
        let prompt = "x".repeat(MAX_PROMPT_BYTES + 1);
        let error = parse(&format!("[[template]]\nname = \"x\"\nprompt = \"{prompt}\"\n"))
            .expect_err("refuse");
        assert!(error.to_string().contains("over the"), "{error}");
    }

    #[test]
    fn applying_a_template_never_takes_the_directory_from_the_file() {
        let templates = parse("[[template]]\nname = \"x\"\n").expect("parse");
        let mut spec = LaunchSpec {
            cwd: PathBuf::from(r"C:\wrong"),
            ..LaunchSpec::default()
        };
        templates[0].apply(Path::new(r"C:\repos\shop"), &mut spec);
        assert_eq!(spec.cwd, PathBuf::from(r"C:\repos\shop"));
    }
}
