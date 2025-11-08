use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::{ColorChoice, PagerMode};

#[derive(Debug, Deserialize)]
pub struct BatchPlan {
    pub steps: Vec<PlanEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum PlanEntry {
    Replace(ReplacePlan),
    Normalize(NormalizePlan),
}

impl PlanEntry {
    pub fn kind(&self) -> &'static str {
        match self {
            PlanEntry::Replace(_) => "replace",
            PlanEntry::Normalize(_) => "normalize",
        }
    }
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct PlanCommon {
    #[serde(default)]
    pub targets: Option<Vec<PathBuf>>,
    #[serde(default)]
    pub globs: Option<Vec<String>>,
    pub encoding: Option<String>,
    pub apply: Option<bool>,
    pub auto_apply: Option<bool>,
    pub no_backup: Option<bool>,
    pub context: Option<usize>,
    pub pager: Option<PagerMode>,
    #[serde(default)]
    pub color: Option<ColorChoice>,
    pub json: Option<bool>,
    pub include_hidden: Option<bool>,
    #[serde(default)]
    pub exclude: Option<Vec<String>>,
    pub undo_log: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
pub struct ReplacePlan {
    #[serde(default)]
    pub common: PlanCommon,
    pub pattern: String,
    pub replacement: Option<String>,
    #[serde(default)]
    pub regex: bool,
    #[serde(default)]
    pub literal: bool,
    #[serde(default)]
    pub diff_only: bool,
    #[serde(default)]
    pub count: Option<usize>,
    #[serde(default)]
    pub expect: Option<usize>,
    #[serde(default)]
    pub after_line: Option<usize>,
    #[serde(default)]
    pub with_stdin: bool,
    #[serde(default)]
    pub with_clipboard: bool,
}

#[derive(Debug, Deserialize, Default)]
pub struct NormalizePlan {
    #[serde(default)]
    pub common: PlanCommon,
    #[serde(default)]
    pub convert_encoding: Option<String>,
    #[serde(default)]
    pub strip_zero_width: Option<bool>,
    #[serde(default)]
    pub strip_control: Option<bool>,
    #[serde(default)]
    pub trim_trailing_space: Option<bool>,
    #[serde(default)]
    pub ensure_eol: Option<bool>,
    #[serde(default)]
    pub report_format: Option<String>,
    #[serde(default)]
    pub scan_encoding: Option<bool>,
    #[serde(default)]
    pub scan_zero_width: Option<bool>,
    #[serde(default)]
    pub scan_control: Option<bool>,
    #[serde(default)]
    pub scan_trailing_space: Option<bool>,
    #[serde(default)]
    pub scan_final_newline: Option<bool>,
}

pub fn load_plan(path: &Path) -> Result<BatchPlan> {
    let data = fs::read(path).with_context(|| format!("reading plan {}", path.display()))?;
    if path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("json"))
        .unwrap_or(false)
    {
        Ok(serde_json::from_slice(&data)?)
    } else {
        Ok(serde_yaml::from_slice(&data)?)
    }
}
