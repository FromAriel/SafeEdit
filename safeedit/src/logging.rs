use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

const LOG_DIR: &str = ".safeedit";
const LOG_FILE: &str = "change_log.jsonl";
const MAX_ENTRIES: usize = 500;

#[derive(Debug, Serialize)]
pub struct ChangeLogEntry<'a> {
    pub timestamp: &'a str,
    pub command: &'a str,
    pub path: &'a Path,
    pub action: &'a str,
    #[serde(rename = "lines")]
    pub line_info: &'a str,
}

pub fn record_change(command: &str, path: &Path, action: &str, line_info: &str) -> Result<()> {
    let log_path = ensure_log_file()?;
    let timestamp = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "unknown".into());
    let entry = ChangeLogEntry {
        timestamp: &timestamp,
        command,
        path,
        action,
        line_info,
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
