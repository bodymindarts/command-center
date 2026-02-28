use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
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
}

#[derive(Debug, Deserialize)]
pub struct AgentConfig {
    pub allowed_tools: Vec<String>,
    #[serde(default = "default_model")]
    pub model: String,
}

fn default_model() -> String {
    "sonnet".to_string()
}

#[derive(Debug, Deserialize)]
pub struct TemplateDef {
    pub prompt: String,
}

impl SkillFile {
    pub fn load(skills_dir: &Path, name: &str) -> Result<Self> {
        let path = skills_dir.join(format!("{name}.toml"));
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read skill file: {}", path.display()))?;
        let skill: Self = toml::from_str(&content)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        Ok(skill)
    }

    pub fn validate_params(&self, params: &HashMap<String, String>) -> Result<()> {
        for p in &self.skill.params {
            if p.required && !params.contains_key(&p.name) {
                bail!("missing required parameter: {}", p.name);
            }
        }
        Ok(())
    }

    pub fn render_prompt(&self, params: &HashMap<String, String>) -> Result<String> {
        let env = minijinja::Environment::new();
        let rendered = env
            .render_str(
                &self.template.prompt,
                minijinja::context! { ..params.clone() },
            )
            .context("failed to render prompt template")?;
        Ok(rendered)
    }
}
