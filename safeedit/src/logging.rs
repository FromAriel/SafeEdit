use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

const LOG_DIR: &str = ".safeedit";
const LOG_FILE: &str = "change_log.jsonl";
const MAX_ENTRIES: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LineSpanKind {
    Modified,
    Added,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LineSpan {
    pub kind: LineSpanKind,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Serialize)]
pub struct ChangeLogEntry<'a> {
    pub timestamp: &'a str,
    pub command: &'a str,
    pub path: &'a str,
    pub action: &'a str,
    #[serde(rename = "lines")]
    pub line_summary: &'a str,
    #[serde(rename = "spans", skip_serializing_if = "Option::is_none")]
    pub spans: Option<&'a [LineSpan]>,
}

#[derive(Debug, Deserialize)]
pub struct LoggedEntry {
    pub timestamp: String,
    pub command: String,
    pub path: String,
    pub action: String,
    #[serde(rename = "lines")]
    pub line_summary: String,
    #[serde(default)]
    pub spans: Vec<LineSpan>,
}

pub fn record_change(
    command: &str,
    path: &Path,
    action: &str,
    line_summary: &str,
    spans: &[LineSpan],
) -> Result<()> {
    let log_path = ensure_log_file()?;
    let timestamp = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "unknown".into());
    let entry = ChangeLogEntry {
        timestamp: &timestamp,
        command,
        path: &path.to_string_lossy(),
        action,
        line_summary,
        spans: (!spans.is_empty()).then_some(spans),
    };
    let json = serde_json::to_string(&entry)?;
    let mut file = OpenOptions::new()
        .append(true)
        .create(true)
        .open(&log_path)
        .with_context(|| format!("opening {log_path:?}"))?;
    writeln!(file, "{json}")?;
    truncate_log(&log_path)?;
    Ok(())
}

pub fn read_recent(limit: usize) -> Result<Vec<LoggedEntry>> {
    let path = PathBuf::from(LOG_DIR).join(LOG_FILE);
    if !path.exists() {
        return Ok(vec![]);
    }
    let file = OpenOptions::new()
        .read(true)
        .open(&path)
        .with_context(|| format!("reading {path:?}"))?;
    let reader = BufReader::new(file);
    let mut lines = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        lines.push(line);
    }
    let start = lines.len().saturating_sub(limit);
    let mut entries = Vec::new();
    for line in &lines[start..] {
        if let Ok(entry) = serde_json::from_str::<LoggedEntry>(line) {
            entries.push(entry);
        }
    }
    Ok(entries)
}

pub fn read_all() -> Result<Vec<LoggedEntry>> {
    let path = PathBuf::from(LOG_DIR).join(LOG_FILE);
    if !path.exists() {
        return Ok(vec![]);
    }
    let file = OpenOptions::new()
        .read(true)
        .open(&path)
        .with_context(|| format!("reading {path:?}"))?;
    let reader = BufReader::new(file);
    let mut entries = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<LoggedEntry>(&line) {
            entries.push(entry);
        }
    }
    Ok(entries)
}

fn ensure_log_file() -> Result<PathBuf> {
    let dir = PathBuf::from(LOG_DIR);
    if !dir.exists() {
        fs::create_dir_all(&dir).with_context(|| format!("creating {dir:?}"))?;
    }
    Ok(dir.join(LOG_FILE))
}

fn truncate_log(path: &Path) -> Result<()> {
    let file = OpenOptions::new()
        .read(true)
        .open(path)
        .with_context(|| format!("reading {path:?}"))?;
    let reader = BufReader::new(file);
    let lines: Vec<_> = reader.lines().collect::<Result<_, _>>()?;
    if lines.len() <= MAX_ENTRIES {
        return Ok(());
    }
    let keep = &lines[lines.len() - MAX_ENTRIES..];
    fs::write(path, keep.join("\n") + "\n")?;
    Ok(())
}
