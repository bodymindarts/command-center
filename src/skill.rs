use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, bail};
use serde::Deserialize;

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
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default)]
    pub base_tools: BaseTools,
    #[serde(default)]
    pub allowed_bash_patterns: Vec<String>,
}

fn default_model() -> String {
    "opus".to_string()
}

#[derive(Debug, Deserialize)]
pub struct TemplateDef {
    #[serde(default)]
    pub system: Option<String>,
    pub prompt: String,
}

impl SkillFile {
    pub fn load(skills_dir: &Path, name: &str) -> anyhow::Result<Self> {
        let path = skills_dir.join(format!("{name}.toml"));
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

    pub fn render_system(&self) -> anyhow::Result<Option<String>> {
        match &self.template.system {
            Some(system) => Ok(Some(system.trim().to_string())),
            None => Ok(None),
        }
    }

    pub fn render_prompt(&self, params: &HashMap<String, String>) -> anyhow::Result<String> {
        let mut merged = HashMap::new();
        for p in &self.skill.params {
            if let Some(default) = &p.default {
                merged.insert(p.name.clone(), default.clone());
            }
        }
        merged.extend(params.clone());

        let env = minijinja::Environment::new();
        let rendered = env
            .render_str(&self.template.prompt, minijinja::context! { ..merged })
            .context("failed to render prompt template")?;
        Ok(rendered)
    }
}
