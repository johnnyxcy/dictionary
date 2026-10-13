use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::model::{ServiceTier, Setup};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

pub const EFFORTS: [(&str, &str); 4] = [
    ("low", "Low"),
    ("medium", "Med"),
    ("high", "High"),
    ("max", "Max"),
];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelOption {
    pub id: String,
    pub model: String,
    pub display_name: String,
    pub efforts: Vec<String>,
    pub default_effort: String,
    pub is_default: bool,
    #[serde(default)]
    pub supports_fast: bool,
}

impl ModelOption {
    pub fn supports(&self, effort: &str) -> bool {
        EFFORTS.iter().any(|(value, _)| *value == effort)
            && self.efforts.iter().any(|value| value == effort)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillSource {
    pub path: PathBuf,
    pub name: String,
    pub hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Catalog {
    pub executable: PathBuf,
    pub cli_version: String,
    pub retrieved_at: String,
    pub configured_model: Option<String>,
    pub models: Vec<ModelOption>,
    pub skills: Vec<SkillSource>,
    #[serde(default)]
    pub skill_diagnostic: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preferences {
    pub version: u32,
    pub model: String,
    pub effort: String,
    #[serde(default)]
    pub fast_mode: bool,
    pub skill_path: PathBuf,
    #[serde(default)]
    pub cli_path: Option<PathBuf>,
    #[serde(default)]
    pub appearance: Appearance,
}

impl Preferences {
    pub fn initial(catalog: &Catalog) -> Self {
        let model = catalog
            .models
            .iter()
            .find(|m| {
                catalog
                    .configured_model
                    .as_ref()
                    .is_some_and(|name| &m.model == name || &m.id == name)
            })
            .or_else(|| catalog.models.iter().find(|m| m.is_default))
            .or_else(|| catalog.models.first());
        Self {
            version: 1,
            fast_mode: false,
            cli_path: Some(catalog.executable.clone()),
            appearance: Appearance::System,
            model: model.map(|m| m.model.clone()).unwrap_or_default(),
            effort: model
                .map(|m| {
                    if m.supports("low") {
                        "low".into()
                    } else if m.supports(&m.default_effort) {
                        m.default_effort.clone()
                    } else {
                        String::new()
                    }
                })
                .unwrap_or_default(),
            // Differing sources require an explicit choice, never an arbitrary copy.
            skill_path: if catalog.skills.len() == 1 {
                catalog.skills[0].path.clone()
            } else {
                PathBuf::new()
            },
        }
    }

    pub fn setup(&self, catalog: &Catalog) -> Result<Setup> {
        ensure!(
            self.version == 1,
            "Settings were created by a newer JDict version"
        );
        // A catalog from another executable cannot validate this connection.
        // Development overrides intentionally take precedence over saved paths.
        let executable = std::env::var_os("DICTIONARY_CLI")
            .map(PathBuf::from)
            .or_else(|| self.cli_path.clone());
        ensure!(
            executable
                .as_ref()
                .is_none_or(|path| path == &catalog.executable),
            "Connect to the selected CLI before choosing a model. Refresh Settings to try again."
        );
        let model = catalog
            .models
            .iter()
            .find(|m| m.model == self.model)
            .context("Choose an available model in Settings")?;
        ensure!(
            model.supports(&self.effort),
            "Choose a supported thinking effort in Settings. Max requires literal max support."
        );
        ensure!(
            !self.fast_mode || model.supports_fast,
            "Fast mode is unavailable for this model. Turn it off or choose a supported model."
        );
        ensure!(
            !self.skill_path.as_os_str().is_empty(),
            "Choose your dictionary SKILL.md in Settings"
        );
        let name = catalog
            .skills
            .iter()
            .find(|s| s.path == self.skill_path)
            .map(|s| s.name.clone())
            .unwrap_or_else(|| "dict".into());
        Ok(Setup {
            executable: catalog.executable.clone(),
            cli_version: catalog.cli_version.clone(),
            model: self.model.clone(),
            effort: self.effort.clone(),
            service_tier: Some(if self.fast_mode {
                ServiceTier::Fast
            } else {
                ServiceTier::Standard
            }),
            skill_path: self.skill_path.clone(),
            skill_name: name,
        })
    }

    pub fn same_generation_settings(&self, other: &Self) -> bool {
        self.model == other.model
            && self.effort == other.effort
            && self.fast_mode == other.fast_mode
            && self.skill_path == other.skill_path
    }
}

#[derive(Clone, Debug, Default)]
pub struct SettingsState {
    pub preferences: Option<Preferences>,
    pub catalog: Option<Catalog>,
    pub cached: bool,
    pub diagnostic: Option<String>,
}

impl SettingsState {
    pub fn setup(&self) -> Result<Setup> {
        self.preferences
            .as_ref()
            .context("Open Settings to finish setup")?
            .setup(
                self.catalog
                    .as_ref()
                    .context("Refresh the model catalog in Settings")?,
            )
    }
}
