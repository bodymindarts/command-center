use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, bail};
use minijinja::context;
use serde::Deserialize;

use crate::harness::HarnessKind;

/// Controls which set of base Bash permissions an agent inherits.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BaseTools {
    /// All git/cargo/nix/shell tools (backwards-compatible default).
    #[default]
    Full,
    /// Only basic read-only shell commands (ls, cat, head, tail, wc, which, pwd).
    Minimal,
    /// No base Bash tools at all — only what's in `allowed_tools`.
    None,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct SkillFile {
    pub skill: SkillDef,
    pub agent: AgentConfig,
    pub template: TemplateDef,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct SkillDef {
    pub name: String,
    pub description: String,
    pub params: Vec<ParamDef>,
}

#[derive(Debug, Deserialize)]
pub struct ParamDef {
    pub name: String,
    #[serde(default)]
    pub required: bool,
    pub default: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct AgentConfig {
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub base_tools: BaseTools,
    #[serde(default)]
    pub allowed_bash_patterns: Vec<String>,
    /// Which agent harness drives this skill's task. Defaults to `Claude`
    /// when unset so existing skill TOMLs don't need updates.
    #[serde(default)]
    pub harness: Option<HarnessKind>,
}

#[derive(Debug, Deserialize)]
pub struct TemplateDef {
    #[serde(default)]
    pub system: Option<String>,
    pub prompt: String,
}

/// Names of every skill defined in `skills_dir`, sorted.
pub fn available_skills(skills_dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(skills_dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "toml"))
        .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .collect();
    names.sort();
    names
}

impl SkillFile {
    pub fn load(skills_dir: &Path, name: &str) -> anyhow::Result<Self> {
        let path = skills_dir.join(format!("{name}.toml"));
        // Reject an unknown skill by name rather than letting a "no such file"
        // surface later — the operator needs to know which names are valid.
        if !path.is_file() {
            return Err(crate::suggest::unknown_name_error(
                "skill",
                name,
                available_skills(skills_dir),
            ));
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read skill file: {}", path.display()))?;
        let skill: Self = toml::from_str(&content)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        Ok(skill)
    }

    pub fn validate_params(&self, params: &HashMap<String, String>) -> anyhow::Result<()> {
        for p in &self.skill.params {
            if p.required && !params.contains_key(&p.name) {
                bail!("missing required parameter: {}", p.name);
            }
        }
        Ok(())
    }

    pub fn render_system(
        &self,
        params: &HashMap<String, String>,
        repo_root: &Path,
    ) -> anyhow::Result<Option<String>> {
        match &self.template.system {
            Some(system) => {
                let mut merged = HashMap::new();
                for p in &self.skill.params {
                    if let Some(default) = &p.default {
                        merged.insert(p.name.clone(), default.clone());
                    }
                }
                merged.extend(params.clone());
                merged.insert(
                    "clat_bin".to_string(),
                    repo_root.join("bin/clat").display().to_string(),
                );

                let env = minijinja::Environment::new();
                let rendered = env
                    .render_str(system, context! { ..merged })
                    .context("failed to render system template")?;
                Ok(Some(rendered.trim().to_string()))
            }
            None => Ok(None),
        }
    }

    pub fn render_prompt(
        &self,
        params: &HashMap<String, String>,
        repo_root: &Path,
    ) -> anyhow::Result<String> {
        let mut merged = HashMap::new();
        for p in &self.skill.params {
            if let Some(default) = &p.default {
                merged.insert(p.name.clone(), default.clone());
            }
        }
        merged.extend(params.clone());
        merged.insert(
            "clat_bin".to_string(),
            repo_root.join("bin/clat").display().to_string(),
        );

        let env = minijinja::Environment::new();
        let rendered = env
            .render_str(&self.template.prompt, context! { ..merged })
            .context("failed to render prompt template")?;
        Ok(rendered)
    }
}
