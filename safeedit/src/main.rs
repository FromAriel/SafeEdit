use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use arboard::Clipboard;
use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum, ValueHint, value_parser};
use diffy::{Patch as DiffPatch, apply as apply_patch};
use encoding_rs::Encoding;
use is_terminal::IsTerminal;
use serde::{Deserialize, Serialize};
use serde_json::{Map as JsonMap, Value as JsonValue, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use walkdir::WalkDir;

mod batch;
mod commands;
mod diff;
mod encoding;
mod files;
mod logging;
mod normalize;
mod patch;
mod review;
mod transform;
use commands::{BlockOptions, RenameOptions, ReplaceOptions, run_block, run_rename, run_replace};
use encoding::{DecodedText, EncodingStrategy};
use files::{FileEntry, FileMetadata};
use logging::{LineSpan, LineSpanKind, record_change};
use patch::{FilePatch, PatchKind, load_file_patches};
use transform::TransformResult;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ValueEnum, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
enum ColorChoice {
    #[default]
    Auto,
    Always,
    Never,
}

impl ColorChoice {
    fn should_color(self) -> bool {
        match self {
            ColorChoice::Always => true,
            ColorChoice::Never => false,
            ColorChoice::Auto => io::stdout().is_terminal(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ValueEnum, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum PagerMode {
    #[default]
    Auto,
    Always,
    Never,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ValueEnum, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
enum LineEndingChoice {
    #[default]
    Auto,
    Lf,
    Crlf,
    Cr,
}

impl LineEndingChoice {
    fn resolve(self, existing: Option<LineEndingStyle>) -> LineEndingStyle {
        match self {
            LineEndingChoice::Auto => existing.unwrap_or(system_default_line_ending()),
            LineEndingChoice::Lf => LineEndingStyle::Lf,
            LineEndingChoice::Crlf => LineEndingStyle::Crlf,
            LineEndingChoice::Cr => LineEndingStyle::Cr,
        }
    }
}

fn system_default_line_ending() -> LineEndingStyle {
    if cfg!(windows) {
        LineEndingStyle::Crlf
    } else {
        LineEndingStyle::Lf
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    run(cli)
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Replace(cmd) => handle_replace(cmd)?,
        Command::Apply(cmd) => handle_apply(cmd)?,
        Command::Block(cmd) => handle_block(cmd)?,
        Command::Rename(cmd) => handle_rename(cmd)?,
        Command::Review(cmd) => handle_review(cmd)?,
        Command::Normalize(cmd) => handle_normalize(cmd)?,
        Command::Script(cmd) => handle_script(cmd)?,
        Command::Batch(cmd) => handle_batch(cmd)?,
        Command::Log(cmd) => handle_log(cmd)?,
        Command::Report(cmd) => handle_report(cmd)?,
        Command::Cleanup(cmd) => handle_cleanup(cmd)?,
        Command::Write(cmd) => handle_write(cmd)?,
    }

    Ok(())
}

fn handle_replace(cmd: ReplaceCommand) -> Result<()> {
    let colorize = cmd.common.color.should_color();
    let diff_config = cmd.common.diff_display_config(colorize);
    let entries = resolve_entries(&cmd.common)?;
    let encoding = resolve_encoding_strategy(&cmd.common)?;
    let literal_mode = cmd.literal || !cmd.regex;
    let pattern = if literal_mode {
        regex::escape(&cmd.pattern)
    } else {
        cmd.pattern.clone()
    };
    let (replacement_text, replacement_source) = resolve_replacement_text(&cmd)?;
    let replacement_len = replacement_text.chars().count();
    let replace_options = ReplaceOptions {
        pattern,
        replacement: replacement_text.clone(),
        allow_captures: !literal_mode,
        count: cmd.count,
        expect: cmd.expect,
        after_line: cmd.after_line,
    };
    if cmd.diff_only {
        println!("diff-only mode enabled: changes will not be written even with --apply.");
    }
    let apply_mode = cmd.common.apply && !cmd.diff_only;
    print_command_summary(
        "replace",
        &cmd.common,
        &encoding,
        &entries,
        &[
            format!("pattern={}", cmd.pattern),
            format!("replacement_source={replacement_source}"),
            format!("replacement_length={replacement_len} chars"),
            format!("mode={}", if literal_mode { "literal" } else { "regex" }),
            format!("count={:?}", cmd.count),
            format!("expect={:?}", cmd.expect),
            format!("after_line={:?}", cmd.after_line),
            format!("diff_only={}", cmd.diff_only),
        ],
    );
    let mut apply_all = cmd.common.auto_apply && apply_mode;
    let mut stats = CommandStats::default();
    for entry in &entries {
        let Some(result) = run_replace(entry, &encoding, &replace_options)? else {
            stats.no_op += 1;
            if apply_mode {
                log_change(
                    &cmd.common,
                    "replace",
                    &entry.path,
                    "no-op",
                    "no matches",
                    &[],
                    Some(status_extra(false, false)),
                );
            } else {
                emit_json_diff_event(
                    &cmd.common,
                    "replace",
                    &entry.path,
                    "no-op",
                    "no matches",
                    &[],
                    Some(status_extra(false, true)),
                );
            }
            continue;
        };

        let line_summary = diff::summarize_lines(&result.decoded.text, &result.new_text);
        let line_spans = diff::collect_line_spans(&result.decoded.text, &result.new_text);
        println!("--- preview: {} ---", entry.path.display());
        diff::display_diff(&result.decoded.text, &result.new_text, &diff_config)?;

        if !apply_mode {
            stats.dry_run += 1;
            if cmd.diff_only {
                println!("diff-only: rerun without --diff-only to write this change.");
            } else {
                println!("dry-run: rerun with --apply to write this change.");
            }
            let mut extra = status_extra(false, true);
            if cmd.diff_only {
                extra.insert("diff_only".into(), JsonValue::Bool(true));
            }
            log_change(
                &cmd.common,
                "replace",
                &entry.path,
                "dry-run",
                &line_summary,
                &line_spans,
                Some(extra),
            );
            continue;
        }

        let decision = if apply_all {
            ApprovalDecision::Apply
        } else {
            prompt_approval(&entry.path)?
        };

        match decision {
            ApprovalDecision::Apply => {
                apply_transform(
                    entry,
                    &result,
                    None,
                    cmd.common.undo_log.as_deref(),
                    cmd.common.no_backup,
                )?;
                stats.applied += 1;
                log_change(
                    &cmd.common,
                    "replace",
                    &entry.path,
                    "applied",
                    &line_summary,
                    &line_spans,
                    Some(status_extra(true, false)),
                );
            }
            ApprovalDecision::ApplyAll => {
                apply_all = true;
                apply_transform(
                    entry,
                    &result,
                    None,
                    cmd.common.undo_log.as_deref(),
                    cmd.common.no_backup,
                )?;
                stats.applied += 1;
                log_change(
                    &cmd.common,
                    "replace",
                    &entry.path,
                    "applied",
                    &line_summary,
                    &line_spans,
                    Some(status_extra(true, false)),
                );
            }
            ApprovalDecision::Skip => {
                println!("skipped {}", entry.path.display());
                stats.skipped += 1;
                log_change(
                    &cmd.common,
                    "replace",
                    &entry.path,
                    "skipped",
                    &line_summary,
                    &line_spans,
                    Some(status_extra(false, false)),
                );
            }
            ApprovalDecision::Quit => {
                println!("stopping after user request.");
                stats.skipped += 1;
                break;
            }
        }
    }
    stats.print("replace");
    Ok(())
}

fn handle_apply(cmd: ApplyCommand) -> Result<()> {
    let colorize = cmd.common.color.should_color();
    let diff_config = cmd.common.diff_display_config(colorize);
    let encoding = resolve_encoding_strategy(&cmd.common)?;
    let root_dir = resolve_patch_root(cmd.root.as_ref())?;
    let mut work_items = collect_patch_work(&cmd.patch_files, &root_dir)?;
    if work_items.is_empty() {
        println!("no applicable patch hunks to review.");
        return Ok(());
    }
    let summary_entries = summarize_work_items(&work_items);
    let details = vec![
        format!("patch files: {}", format_patch_sources(&cmd.patch_files)),
        format!("root: {}", root_dir.display()),
    ];
    print_command_summary("apply", &cmd.common, &encoding, &summary_entries, &details);

    let apply_mode = cmd.common.apply;
    let mut apply_all = cmd.common.auto_apply && apply_mode;
    let mut stats = CommandStats::default();

    'outer: for work in work_items.drain(..) {
        let display_label = match (&work.old_path, &work.new_path) {
            (Some(old), Some(new)) if old != new => {
                format!("{} -> {}", old.display(), new.display())
            }
            (_, Some(new)) => new.display().to_string(),
            (Some(old), None) => old.display().to_string(),
            _ => "(unknown path)".to_string(),
        };
        let action = match work.patch.kind {
            PatchKind::Modify => "modify",
            PatchKind::Create => "create",
            PatchKind::Delete => "delete",
            PatchKind::Rename => "rename",
        };
        println!(
            "--- patch {}#{} ({display_label}) [{action}] ---",
            work.patch.source.display(),
            work.patch.index
        );

        match work.patch.kind {
            PatchKind::Modify => {
                let path = work
                    .new_path
                    .as_ref()
                    .or(work.old_path.as_ref())
                    .cloned()
                    .context("modify patch missing path")?;
                let bytes =
                    fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
                let file_len = bytes.len() as u64;
                let decoded = encoding.decode(&bytes);
                let parsed_patch = DiffPatch::from_str(&work.patch.patch_text).map_err(|err| {
                    anyhow!(
                        "failed to re-parse patch {}#{} during apply: {err}",
                        work.patch.source.display(),
                        work.patch.index
                    )
                })?;
                let patched = apply_patch_preserving_newlines(&decoded.text, &parsed_patch)
                    .map_err(|err| {
                        anyhow!(
                            "failed to apply patch {}#{} to {}: {err}",
                            work.patch.source.display(),
                            work.patch.index,
                            path.display()
                        )
                    })?;

                let entry = FileEntry {
                    path: path.clone(),
                    metadata: FileMetadata {
                        len: file_len,
                        is_probably_binary: false,
                    },
                };

                if patched == decoded.text {
                    println!(
                        "patch {}#{} made no changes to {}",
                        work.patch.source.display(),
                        work.patch.index,
                        entry.path.display()
                    );
                    stats.no_op += 1;
                    let summary = diff::summarize_lines(&decoded.text, &patched);
                    let spans = diff::collect_line_spans(&decoded.text, &patched);
                    if apply_mode {
                        log_change(
                            &cmd.common,
                            "apply",
                            &entry.path,
                            "no-op",
                            &summary,
                            &spans,
                            Some(status_with_patch(false, false, PatchKind::Modify)),
                        );
                    } else {
                        emit_json_diff_event(
                            &cmd.common,
                            "apply",
                            &entry.path,
                            "no-op",
                            &summary,
                            &spans,
                            Some(status_with_patch(false, true, PatchKind::Modify)),
                        );
                    }
                    continue;
                }

                diff::display_diff(&decoded.text, &patched, &diff_config)?;
                let result = TransformResult {
                    decoded,
                    new_text: patched,
                };
                let line_summary = diff::summarize_lines(&result.decoded.text, &result.new_text);
                let line_spans = diff::collect_line_spans(&result.decoded.text, &result.new_text);

                if !apply_mode {
                    stats.dry_run += 1;
                    println!("dry-run: rerun with --apply to write this change.");
                    log_change(
                        &cmd.common,
                        "apply",
                        &entry.path,
                        "dry-run",
                        &line_summary,
                        &line_spans,
                        Some(status_with_patch(false, true, PatchKind::Modify)),
                    );
                    continue;
                }

                let decision = if apply_all {
                    ApprovalDecision::Apply
                } else {
                    prompt_approval(&entry.path)?
                };

                match decision {
                    ApprovalDecision::Apply => {
                        apply_transform(
                            &entry,
                            &result,
                            None,
                            cmd.common.undo_log.as_deref(),
                            cmd.common.no_backup,
                        )?;
                        stats.applied += 1;
                        log_change(
                            &cmd.common,
                            "apply",
                            &entry.path,
                            "applied",
                            &line_summary,
                            &line_spans,
                            Some(status_with_patch(true, false, PatchKind::Modify)),
                        );
                    }
                    ApprovalDecision::ApplyAll => {
                        apply_all = true;
                        apply_transform(
                            &entry,
                            &result,
                            None,
                            cmd.common.undo_log.as_deref(),
                            cmd.common.no_backup,
                        )?;
                        stats.applied += 1;
                        log_change(
                            &cmd.common,
                            "apply",
                            &entry.path,
                            "applied",
                            &line_summary,
                            &line_spans,
                            Some(status_with_patch(true, false, PatchKind::Modify)),
                        );
                    }
                    ApprovalDecision::Skip => {
                        println!("skipped {}", entry.path.display());
                        stats.skipped += 1;
                        log_change(
                            &cmd.common,
                            "apply",
                            &entry.path,
                            "skipped",
                            &line_summary,
                            &line_spans,
                            Some(status_with_patch(false, false, PatchKind::Modify)),
                        );
                    }
                    ApprovalDecision::Quit => {
                        println!("stopping after user request.");
                        stats.skipped += 1;
                        break 'outer;
                    }
                }
            }
            PatchKind::Create => {
                let path = work
                    .new_path
                    .as_ref()
                    .cloned()
                    .context("create patch missing target path")?;
                if path.exists() {
                    bail!(
                        "refusing to create {} because it already exists",
                        path.display()
                    );
                }
                let parsed_patch = DiffPatch::from_str(&work.patch.patch_text).map_err(|err| {
                    anyhow!(
                        "failed to re-parse patch {}#{} during apply: {err}",
                        work.patch.source.display(),
                        work.patch.index
                    )
                })?;
                let base_text = String::new();
                let new_text =
                    apply_patch_preserving_newlines(&base_text, &parsed_patch).map_err(|err| {
                        anyhow!(
                            "failed to apply patch {}#{} for new file {}: {err}",
                            work.patch.source.display(),
                            work.patch.index,
                            path.display()
                        )
                    })?;
                if new_text == base_text {
                    println!(
                        "patch {}#{} produced no content for {}; skipping",
                        work.patch.source.display(),
                        work.patch.index,
                        path.display()
                    );
                    stats.no_op += 1;
                    let summary = diff::summarize_lines(&base_text, &new_text);
                    let spans = diff::collect_line_spans(&base_text, &new_text);
                    emit_json_diff_event(
                        &cmd.common,
                        "apply",
                        &path,
                        "no-op",
                        &summary,
                        &spans,
                        Some(status_with_patch(false, !apply_mode, PatchKind::Create)),
                    );
                    continue;
                }
                diff::display_diff(&base_text, &new_text, &diff_config)?;
                let decision = if apply_mode {
                    if apply_all {
                        ApprovalDecision::Apply
                    } else {
                        prompt_approval(&path)?
                    }
                } else {
                    ApprovalDecision::Skip
                };
                let line_summary = diff::summarize_lines(&base_text, &new_text);
                let line_spans = diff::collect_line_spans(&base_text, &new_text);
                if !apply_mode {
                    stats.dry_run += 1;
                    println!("dry-run: rerun with --apply to create this file.");
                    log_change(
                        &cmd.common,
                        "apply",
                        &path,
                        "dry-run",
                        &line_summary,
                        &line_spans,
                        Some(status_with_patch(false, true, PatchKind::Create)),
                    );
                    continue;
                }
                match decision {
                    ApprovalDecision::Apply => {
                        write_new_file(
                            &path,
                            &new_text,
                            &encoding,
                            cmd.common.undo_log.as_deref(),
                            cmd.common.no_backup,
                        )?;
                        stats.applied += 1;
                        log_change(
                            &cmd.common,
                            "apply",
                            &path,
                            "applied",
                            &line_summary,
                            &line_spans,
                            Some(status_with_patch(true, false, PatchKind::Create)),
                        );
                    }
                    ApprovalDecision::ApplyAll => {
                        apply_all = true;
                        write_new_file(
                            &path,
                            &new_text,
                            &encoding,
                            cmd.common.undo_log.as_deref(),
                            cmd.common.no_backup,
                        )?;
                        stats.applied += 1;
                        log_change(
                            &cmd.common,
                            "apply",
                            &path,
                            "applied",
                            &line_summary,
                            &line_spans,
                            Some(status_with_patch(true, false, PatchKind::Create)),
                        );
                    }
                    ApprovalDecision::Skip => {
                        println!("skipped {}", path.display());
                        stats.skipped += 1;
                        log_change(
                            &cmd.common,
                            "apply",
                            &path,
                            "skipped",
                            &line_summary,
                            &line_spans,
                            Some(status_with_patch(false, false, PatchKind::Create)),
                        );
                    }
                    ApprovalDecision::Quit => {
                        println!("stopping after user request.");
                        stats.skipped += 1;
                        break 'outer;
                    }
                }
            }
            PatchKind::Delete => {
                let path = work
                    .old_path
                    .as_ref()
                    .cloned()
                    .context("delete patch missing source path")?;
                let bytes =
                    fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
                let decoded = encoding.decode(&bytes);
                let parsed_patch = DiffPatch::from_str(&work.patch.patch_text).map_err(|err| {
                    anyhow!(
                        "failed to re-parse patch {}#{} during apply: {err}",
                        work.patch.source.display(),
                        work.patch.index
                    )
                })?;
                let new_text = apply_patch_preserving_newlines(&decoded.text, &parsed_patch)
                    .map_err(|err| {
                        anyhow!(
                            "failed to apply delete patch {}#{} to {}: {err}",
                            work.patch.source.display(),
                            work.patch.index,
                            path.display()
                        )
                    })?;
                if !new_text.is_empty() {
                    bail!(
                        "delete patch {}#{} for {} did not result in empty content",
                        work.patch.source.display(),
                        work.patch.index,
                        path.display()
                    );
                }
                diff::display_diff(&decoded.text, &new_text, &diff_config)?;
                let line_summary = diff::summarize_lines(&decoded.text, &new_text);
                let line_spans = diff::collect_line_spans(&decoded.text, &new_text);
                if !apply_mode {
                    stats.dry_run += 1;
                    println!("dry-run: rerun with --apply to delete this file.");
                    log_change(
                        &cmd.common,
                        "apply",
                        &path,
                        "dry-run",
                        &line_summary,
                        &line_spans,
                        Some(status_with_patch(false, true, PatchKind::Delete)),
                    );
                    continue;
                }
                let decision = if apply_all {
                    ApprovalDecision::Apply
                } else {
                    prompt_approval(&path)?
                };
                match decision {
                    ApprovalDecision::Apply => {
                        delete_file_with_undo(
                            &path,
                            &decoded.text,
                            cmd.common.undo_log.as_deref(),
                            cmd.common.no_backup,
                        )?;
                        stats.applied += 1;
                        log_change(
                            &cmd.common,
                            "apply",
                            &path,
                            "applied",
                            &line_summary,
                            &line_spans,
                            Some(status_with_patch(true, false, PatchKind::Delete)),
                        );
                    }
                    ApprovalDecision::ApplyAll => {
                        apply_all = true;
                        delete_file_with_undo(
                            &path,
                            &decoded.text,
                            cmd.common.undo_log.as_deref(),
                            cmd.common.no_backup,
                        )?;
                        stats.applied += 1;
                        log_change(
                            &cmd.common,
                            "apply",
                            &path,
                            "applied",
                            &line_summary,
                            &line_spans,
                            Some(status_with_patch(true, false, PatchKind::Delete)),
                        );
                    }
                    ApprovalDecision::Skip => {
                        println!("skipped {}", path.display());
                        stats.skipped += 1;
                        log_change(
                            &cmd.common,
                            "apply",
                            &path,
                            "skipped",
                            &line_summary,
                            &line_spans,
                            Some(status_with_patch(false, false, PatchKind::Delete)),
                        );
                    }
                    ApprovalDecision::Quit => {
                        println!("stopping after user request.");
                        stats.skipped += 1;
                        break 'outer;
                    }
                }
            }
            PatchKind::Rename => {
                let old_path = work
                    .old_path
                    .as_ref()
                    .cloned()
                    .context("rename patch missing source path")?;
                let new_path = work
                    .new_path
                    .as_ref()
                    .cloned()
                    .context("rename patch missing destination path")?;
                if new_path != old_path && new_path.exists() {
                    bail!(
                        "refusing to overwrite existing {} during rename",
                        new_path.display()
                    );
                }
                let bytes = fs::read(&old_path)
                    .with_context(|| format!("reading {}", old_path.display()))?;
                let decoded = encoding.decode(&bytes);
                let parsed_patch = DiffPatch::from_str(&work.patch.patch_text).map_err(|err| {
                    anyhow!(
                        "failed to re-parse patch {}#{} during apply: {err}",
                        work.patch.source.display(),
                        work.patch.index
                    )
                })?;
                let new_text = apply_patch_preserving_newlines(&decoded.text, &parsed_patch)
                    .map_err(|err| {
                        anyhow!(
                            "failed to apply rename patch {}#{} for {} -> {}: {err}",
                            work.patch.source.display(),
                            work.patch.index,
                            old_path.display(),
                            new_path.display()
                        )
                    })?;
                let content_changed = new_text != decoded.text;
                if content_changed {
                    diff::display_diff(&decoded.text, &new_text, &diff_config)?;
                } else {
                    println!("(rename only; no textual diff)");
                }
                let line_summary = if content_changed {
                    diff::summarize_lines(&decoded.text, &new_text)
                } else {
                    "rename-only".to_string()
                };
                let line_spans = if content_changed {
                    diff::collect_line_spans(&decoded.text, &new_text)
                } else {
                    Vec::new()
                };

                if !apply_mode {
                    stats.dry_run += 1;
                    println!("dry-run: rerun with --apply to rename this file.");
                    log_change(
                        &cmd.common,
                        "apply",
                        &new_path,
                        "dry-run (rename)",
                        &line_summary,
                        &line_spans,
                        Some(status_with_patch(false, true, PatchKind::Rename)),
                    );
                    continue;
                }

                let decision = if apply_all {
                    ApprovalDecision::Apply
                } else {
                    prompt_approval(&new_path)?
                };

                match decision {
                    ApprovalDecision::Apply => {
                        let decoded_for_dest = decoded.clone();
                        let dest_entry = FileEntry {
                            path: new_path.clone(),
                            metadata: FileMetadata {
                                len: new_text.len() as u64,
                                is_probably_binary: false,
                            },
                        };
                        let result = TransformResult {
                            decoded: decoded_for_dest.clone(),
                            new_text: new_text.clone(),
                        };
                        apply_transform(
                            &dest_entry,
                            &result,
                            Some(decoded_for_dest.decision.encoding),
                            cmd.common.undo_log.as_deref(),
                            cmd.common.no_backup,
                        )?;
                        delete_file_with_undo(
                            &old_path,
                            &decoded.text,
                            cmd.common.undo_log.as_deref(),
                            cmd.common.no_backup,
                        )?;
                        stats.applied += 1;
                        log_change(
                            &cmd.common,
                            "apply",
                            &new_path,
                            "applied (rename)",
                            &line_summary,
                            &line_spans,
                            Some(status_with_patch(true, false, PatchKind::Rename)),
                        );
                        log_change(
                            &cmd.common,
                            "apply",
                            &old_path,
                            "deleted (rename)",
                            "entire file removed",
                            &[],
                            Some(status_with_patch(true, false, PatchKind::Rename)),
                        );
                    }
                    ApprovalDecision::ApplyAll => {
                        apply_all = true;
                        let decoded_for_dest = decoded.clone();
                        let dest_entry = FileEntry {
                            path: new_path.clone(),
                            metadata: FileMetadata {
                                len: new_text.len() as u64,
                                is_probably_binary: false,
                            },
                        };
                        let result = TransformResult {
                            decoded: decoded_for_dest.clone(),
                            new_text: new_text.clone(),
                        };
                        apply_transform(
                            &dest_entry,
                            &result,
                            Some(decoded_for_dest.decision.encoding),
                            cmd.common.undo_log.as_deref(),
                            cmd.common.no_backup,
                        )?;
                        delete_file_with_undo(
                            &old_path,
                            &decoded.text,
                            cmd.common.undo_log.as_deref(),
                            cmd.common.no_backup,
                        )?;
                        stats.applied += 1;
                        log_change(
                            &cmd.common,
                            "apply",
                            &new_path,
                            "applied (rename)",
                            &line_summary,
                            &line_spans,
                            Some(status_with_patch(true, false, PatchKind::Rename)),
                        );
                        log_change(
                            &cmd.common,
                            "apply",
                            &old_path,
                            "deleted (rename)",
                            "entire file removed",
                            &[],
                            Some(status_with_patch(true, false, PatchKind::Rename)),
                        );
                    }
                    ApprovalDecision::Skip => {
                        println!("skipped rename to {}", new_path.display());
                        stats.skipped += 1;
                        log_change(
                            &cmd.common,
                            "apply",
                            &new_path,
                            "skipped (rename)",
                            &line_summary,
                            &line_spans,
                            Some(status_with_patch(false, false, PatchKind::Rename)),
                        );
                    }
                    ApprovalDecision::Quit => {
                        println!("stopping after user request.");
                        stats.skipped += 1;
                        break 'outer;
                    }
                }
            }
        }
    }

    stats.print("apply");
    Ok(())
}

struct PatchWork {
    patch: FilePatch,
    old_path: Option<PathBuf>,
    new_path: Option<PathBuf>,
}

fn collect_patch_work(patch_files: &[PathBuf], root: &Path) -> Result<Vec<PatchWork>> {
    let mut items = Vec::new();
    for patch_path in patch_files {
        let patches = load_file_patches(patch_path)?;
        if patches.is_empty() {
            println!("warning: {} contained no patch hunks", patch_path.display());
        }
        for patch in patches {
            let old_abs = patch
                .old_path
                .as_ref()
                .map(|p| resolve_patch_target(root, p));
            let new_abs = patch
                .new_path
                .as_ref()
                .map(|p| resolve_patch_target(root, p));
            items.push(PatchWork {
                patch,
                old_path: old_abs,
                new_path: new_abs,
            });
        }
    }
    Ok(items)
}

fn summarize_work_items(work_items: &[PatchWork]) -> Vec<FileEntry> {
    let mut map = BTreeMap::new();
    for work in work_items {
        let path = work.new_path.as_ref().or(work.old_path.as_ref()).cloned();
        let Some(path) = path else { continue };
        let len = fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
        map.entry(path.clone()).or_insert(FileEntry {
            path,
            metadata: FileMetadata {
                len,
                is_probably_binary: false,
            },
        });
    }
    map.into_values().collect()
}

fn resolve_patch_root(root: Option<&PathBuf>) -> Result<PathBuf> {
    match root {
        Some(path) => {
            fs::canonicalize(path).with_context(|| format!("resolving root {}", path.display()))
        }
        None => std::env::current_dir().context("determining working directory"),
    }
}

fn resolve_patch_target(root: &Path, relative: &Path) -> PathBuf {
    if relative.is_absolute() {
        relative.to_path_buf()
    } else {
        root.join(relative)
    }
}

fn format_patch_sources(patch_files: &[PathBuf]) -> String {
    patch_files
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn resolve_replacement_text(cmd: &ReplaceCommand) -> Result<(String, &'static str)> {
    if let Some(text) = &cmd.replacement {
        return Ok((text.clone(), "literal"));
    }
    if cmd.with_stdin {
        let text = read_replacement_from_stdin()?;
        return Ok((text, "stdin"));
    }
    if cmd.with_clipboard {
        let text = read_replacement_from_clipboard()?;
        return Ok((text, "clipboard"));
    }
    if let Some(tag) = &cmd.with_here {
        let text = read_heredoc_input(tag, "replacement")?;
        return Ok((text, "heredoc"));
    }
    bail!("replacement text required; use --with, --with-stdin, --with-clipboard, or --with-here");
}

fn resolve_body_from_sources(
    literal_lines: &[String],
    body_file: &Option<PathBuf>,
    with_stdin: bool,
    with_clipboard: bool,
    heredoc_tag: &Option<String>,
    description: &str,
) -> Result<(String, &'static str)> {
    if !literal_lines.is_empty() {
        let text = literal_lines.join("\n");
        return Ok((text, "literal"));
    }
    if let Some(path) = body_file {
        let text = fs::read_to_string(path)
            .with_context(|| format!("reading {description} from {}", path.display()))?;
        return Ok((text, "file"));
    }
    if let Some(tag) = heredoc_tag {
        let text = read_heredoc_input(tag, description)?;
        return Ok((text, "heredoc"));
    }
    if with_stdin {
        let text = read_replacement_from_stdin()?;
        return Ok((text, "stdin"));
    }
    if with_clipboard {
        let text = read_replacement_from_clipboard()?;
        return Ok((text, "clipboard"));
    }
    bail!("{description} required; use --body, --body-file, --with-stdin, or --with-clipboard");
}

fn resolve_block_body(cmd: &BlockCommand) -> Result<(String, &'static str)> {
    resolve_body_from_sources(
        &cmd.body,
        &cmd.body_file,
        cmd.with_stdin,
        cmd.with_clipboard,
        &cmd.body_here,
        "block body",
    )
}

fn read_replacement_from_stdin() -> Result<String> {
    let mut buf = String::new();
    io::stdin()
        .read_to_string(&mut buf)
        .context("reading replacement text from stdin")?;
    Ok(buf)
}

fn read_replacement_from_clipboard() -> Result<String> {
    let mut clipboard = Clipboard::new().context("opening clipboard")?;
    clipboard
        .get_text()
        .context("reading clipboard text for replacement")
}

fn read_heredoc_input(tag: &str, description: &str) -> Result<String> {
    if tag.trim().is_empty() {
        bail!("heredoc terminator cannot be empty");
    }
    println!("Enter {description}; finish with a line containing only {tag}.");
    let mut buf = String::new();
    let mut line = String::new();
    loop {
        line.clear();
        let bytes = io::stdin()
            .read_line(&mut line)
            .context("reading heredoc input")?;
        if bytes == 0 {
            bail!("stdin closed before heredoc terminator '{tag}'");
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed == tag {
            break;
        }
        buf.push_str(&line);
    }
    Ok(buf)
}

fn handle_block(cmd: BlockCommand) -> Result<()> {
    let colorize = cmd.common.color.should_color();
    let diff_config = cmd.common.diff_display_config(colorize);
    let entries = resolve_entries(&cmd.common)?;
    let encoding = resolve_encoding_strategy(&cmd.common)?;
    let (body_text, body_source) = resolve_block_body(&cmd)?;
    print_command_summary(
        "block",
        &cmd.common,
        &encoding,
        &entries,
        &[
            format!("start_marker={}", cmd.start_marker),
            format!("end_marker={}", cmd.end_marker),
            format!("mode={:?}", cmd.mode),
            format!("body_source={body_source}"),
            format!("body_length={} chars", body_text.chars().count()),
        ],
    );
    let options = BlockOptions {
        start_marker: cmd.start_marker.clone(),
        end_marker: cmd.end_marker.clone(),
        mode: cmd.mode,
        body: body_text,
    };
    let apply_mode = cmd.common.apply;
    let mut apply_all = cmd.common.auto_apply && apply_mode;
    let mut stats = CommandStats::default();
    for entry in &entries {
        let Some(result) = run_block(entry, &encoding, &options)? else {
            stats.no_op += 1;
            log_change(
                &cmd.common,
                "block",
                &entry.path,
                "no-op",
                "no change",
                &[],
                Some(status_extra(false, false)),
            );
            continue;
        };

        let line_summary = diff::summarize_lines(&result.decoded.text, &result.new_text);
        let line_spans = diff::collect_line_spans(&result.decoded.text, &result.new_text);
        println!("--- preview: {} ---", entry.path.display());
        diff::display_diff(&result.decoded.text, &result.new_text, &diff_config)?;

        if !apply_mode {
            stats.dry_run += 1;
            println!("dry-run: rerun with --apply to write this change.");
            log_change(
                &cmd.common,
                "block",
                &entry.path,
                "dry-run",
                &line_summary,
                &line_spans,
                Some(status_extra(false, true)),
            );
            continue;
        }

        let decision = if apply_all {
            ApprovalDecision::Apply
        } else {
            prompt_approval(&entry.path)?
        };

        match decision {
            ApprovalDecision::Apply => {
                apply_transform(
                    entry,
                    &result,
                    None,
                    cmd.common.undo_log.as_deref(),
                    cmd.common.no_backup,
                )?;
                stats.applied += 1;
                log_change(
                    &cmd.common,
                    "block",
                    &entry.path,
                    "applied",
                    &line_summary,
                    &line_spans,
                    Some(status_extra(true, false)),
                );
            }
            ApprovalDecision::ApplyAll => {
                apply_all = true;
                apply_transform(
                    entry,
                    &result,
                    None,
                    cmd.common.undo_log.as_deref(),
                    cmd.common.no_backup,
                )?;
                stats.applied += 1;
                log_change(
                    &cmd.common,
                    "block",
                    &entry.path,
                    "applied",
                    &line_summary,
                    &line_spans,
                    Some(status_extra(true, false)),
                );
            }
            ApprovalDecision::Skip => {
                println!("skipped {}", entry.path.display());
                stats.skipped += 1;
                log_change(
                    &cmd.common,
                    "block",
                    &entry.path,
                    "skipped",
                    &line_summary,
                    &line_spans,
                    Some(status_extra(false, false)),
                );
            }
            ApprovalDecision::Quit => {
                println!("stopping after user request.");
                stats.skipped += 1;
                break;
            }
        }
    }
    stats.print("block");
    Ok(())
}

fn handle_write(cmd: WriteCommand) -> Result<()> {
    let colorize = cmd.common.color.should_color();
    let diff_config = cmd.common.diff_display_config(colorize);
    let encoding = resolve_encoding_strategy(&cmd.common)?;
    let (body_text, body_source) = resolve_body_from_sources(
        &cmd.body,
        &cmd.body_file,
        cmd.with_stdin,
        cmd.with_clipboard,
        &cmd.body_here,
        "write body",
    )?;
    let path = cmd.path.clone();
    let exists = path.exists();
    if exists && !cmd.allow_overwrite {
        bail!(
            "{} already exists; use --allow-overwrite to replace it",
            path.display()
        );
    }

    let mut entry = FileEntry {
        path: path.clone(),
        metadata: FileMetadata {
            len: 0,
            is_probably_binary: false,
        },
    };
    let existing_decoded = if exists {
        let bytes = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        entry.metadata.len = bytes.len() as u64;
        Some(encoding.decode(&bytes))
    } else {
        None
    };
    let target_line_style = cmd.line_ending.resolve(
        existing_decoded
            .as_ref()
            .map(|d| detect_line_ending_style(&d.text)),
    );
    let normalized = normalize_to_lf(&body_text);
    let new_text = restore_from_lf(normalized.into_owned(), target_line_style);
    let old_text = existing_decoded
        .as_ref()
        .map(|d| d.text.clone())
        .unwrap_or_default();

    let patch_kind = if exists {
        PatchKind::Modify
    } else {
        PatchKind::Create
    };
    let mut stats = CommandStats::default();
    let details = vec![
        format!("body_source={body_source}"),
        format!("body_length={} chars", new_text.chars().count()),
        format!("line_ending={:?}", cmd.line_ending),
    ];
    print_command_summary("write", &cmd.common, &encoding, &[entry.clone()], &details);

    if old_text == new_text {
        println!("content already matches {}; nothing to do.", path.display());
        let summary = diff::summarize_lines(&old_text, &new_text);
        let spans = diff::collect_line_spans(&old_text, &new_text);
        log_change(
            &cmd.common,
            "write",
            &path,
            "no-op",
            &summary,
            &spans,
            Some(status_with_patch(false, false, patch_kind)),
        );
        stats.no_op += 1;
        stats.print("write");
        return Ok(());
    }

    diff::display_diff(&old_text, &new_text, &diff_config)?;
    let line_summary = diff::summarize_lines(&old_text, &new_text);
    let line_spans = diff::collect_line_spans(&old_text, &new_text);

    if !cmd.common.apply {
        println!("dry-run: rerun with --apply to write this file.");
        log_change(
            &cmd.common,
            "write",
            &path,
            "dry-run",
            &line_summary,
            &line_spans,
            Some(status_with_patch(false, true, patch_kind)),
        );
        stats.dry_run += 1;
        stats.print("write");
        return Ok(());
    }

    let decision = if cmd.common.auto_apply {
        ApprovalDecision::Apply
    } else {
        prompt_approval(&path)?
    };

    match decision {
        ApprovalDecision::Apply | ApprovalDecision::ApplyAll => {
            let decoded = existing_decoded.unwrap_or_else(|| {
                let decision = encoding.decide(b"");
                DecodedText {
                    text: String::new(),
                    had_errors: false,
                    decision,
                }
            });
            let result = TransformResult {
                decoded,
                new_text: new_text.clone(),
            };
            apply_transform(
                &entry,
                &result,
                Some(result.decoded.decision.encoding),
                cmd.common.undo_log.as_deref(),
                cmd.common.no_backup,
            )?;
            stats.applied += 1;
            log_change(
                &cmd.common,
                "write",
                &path,
                "applied",
                &line_summary,
                &line_spans,
                Some(status_with_patch(true, false, patch_kind)),
            );
        }
        ApprovalDecision::Skip => {
            println!("skipped {}", path.display());
            stats.skipped += 1;
            log_change(
                &cmd.common,
                "write",
                &path,
                "skipped",
                &line_summary,
                &line_spans,
                Some(status_with_patch(false, false, patch_kind)),
            );
        }
        ApprovalDecision::Quit => {
            println!("stopping after user request.");
            stats.skipped += 1;
        }
    }

    stats.print("write");
    Ok(())
}

fn handle_rename(cmd: RenameCommand) -> Result<()> {
    let colorize = cmd.common.color.should_color();
    let diff_config = cmd.common.diff_display_config(colorize);
    let entries = resolve_entries(&cmd.common)?;
    let encoding = resolve_encoding_strategy(&cmd.common)?;
    print_command_summary(
        "rename",
        &cmd.common,
        &encoding,
        &entries,
        &[
            format!("from={}", cmd.from),
            format!("to={}", cmd.to),
            format!("word_boundary={}", cmd.word_boundary),
            format!("case_aware={}", cmd.case_aware),
        ],
    );
    let options = RenameOptions {
        from: cmd.from.clone(),
        to: cmd.to.clone(),
        word_boundary: cmd.word_boundary,
        case_aware: cmd.case_aware,
    };
    let apply_mode = cmd.common.apply;
    let mut apply_all = cmd.common.auto_apply && apply_mode;
    let mut stats = CommandStats::default();
    for entry in &entries {
        let Some(result) = run_rename(entry, &encoding, &options)? else {
            stats.no_op += 1;
            log_change(
                &cmd.common,
                "rename",
                &entry.path,
                "no-op",
                "no change",
                &[],
                Some(status_extra(false, false)),
            );
            continue;
        };
        let line_summary = diff::summarize_lines(&result.decoded.text, &result.new_text);
        let line_spans = diff::collect_line_spans(&result.decoded.text, &result.new_text);
        println!("--- preview: {} ---", entry.path.display());
        diff::display_diff(&result.decoded.text, &result.new_text, &diff_config)?;

        if !apply_mode {
            stats.dry_run += 1;
            println!("dry-run: rerun with --apply to write this change.");
            log_change(
                &cmd.common,
                "rename",
                &entry.path,
                "dry-run",
                &line_summary,
                &line_spans,
                Some(status_extra(false, true)),
            );
            continue;
        }

        let decision = if apply_all {
            ApprovalDecision::Apply
        } else {
            prompt_approval(&entry.path)?
        };

        match decision {
            ApprovalDecision::Apply => {
                apply_transform(
                    entry,
                    &result,
                    None,
                    cmd.common.undo_log.as_deref(),
                    cmd.common.no_backup,
                )?;
                stats.applied += 1;
                log_change(
                    &cmd.common,
                    "rename",
                    &entry.path,
                    "applied",
                    &line_summary,
                    &line_spans,
                    Some(status_extra(true, false)),
                );
            }
            ApprovalDecision::ApplyAll => {
                apply_all = true;
                apply_transform(
                    entry,
                    &result,
                    None,
                    cmd.common.undo_log.as_deref(),
                    cmd.common.no_backup,
                )?;
                stats.applied += 1;
                log_change(
                    &cmd.common,
                    "rename",
                    &entry.path,
                    "applied",
                    &line_summary,
                    &line_spans,
                    Some(status_extra(true, false)),
                );
            }
            ApprovalDecision::Skip => {
                println!("skipped {}", entry.path.display());
                stats.skipped += 1;
                log_change(
                    &cmd.common,
                    "rename",
                    &entry.path,
                    "skipped",
                    &line_summary,
                    &line_spans,
                    Some(status_extra(false, false)),
                );
            }
            ApprovalDecision::Quit => {
                println!("stopping after user request.");
                stats.skipped += 1;
                break;
            }
        }
    }
    stats.print("rename");
    Ok(())
}

fn handle_review(cmd: ReviewCommand) -> Result<()> {
    let entries = resolve_entries(&cmd.common)?;
    let encoding = resolve_encoding_strategy(&cmd.common)?;
    let review_options = review::ReviewOptions::from_input(review::ReviewInput {
        head: cmd.head,
        tail: cmd.tail,
        lines: cmd.lines.as_deref(),
        around: cmd.around.as_deref(),
        follow: cmd.follow,
        step: cmd.step,
        search: cmd.search.as_deref(),
        regex: cmd.regex,
    })?;
    if cmd.follow && entries.len() != 1 {
        bail!("--follow requires exactly one resolved file");
    }
    print_command_summary(
        "review",
        &cmd.common,
        &encoding,
        &entries,
        &[
            format!("head={:?}", cmd.head),
            format!("tail={:?}", cmd.tail),
            format!("lines={:?}", cmd.lines),
            format!("around={:?}", cmd.around),
            format!("follow={}", cmd.follow),
            format!("step={}", cmd.step),
            format!("search={:?}", cmd.search),
            format!("regex={}", cmd.regex),
        ],
    );
    review::run(&entries, &encoding, &review_options)?;
    Ok(())
}

fn handle_normalize(cmd: NormalizeCommand) -> Result<()> {
    let colorize = cmd.common.color.should_color();
    let diff_config = cmd.common.diff_display_config(colorize);
    let entries = resolve_entries(&cmd.common)?;
    let encoding = resolve_encoding_strategy(&cmd.common)?;
    let report_format = ReportFormat::from_str(&cmd.report_format)?;
    let convert_encoding = if let Some(label) = cmd.convert_encoding.as_deref() {
        let trimmed = label.trim();
        let encoding = Encoding::for_label(trimmed.as_bytes())
            .ok_or_else(|| anyhow!("unknown convert-encoding '{trimmed}'"))?;
        Some((encoding, trimmed.to_string()))
    } else {
        None
    };

    let any_scan = cmd.scan_encoding
        || cmd.scan_zero_width
        || cmd.scan_control
        || cmd.scan_trailing_space
        || cmd.scan_final_newline;
    let detect_zero_width = if any_scan { cmd.scan_zero_width } else { true };
    let detect_control = if any_scan { cmd.scan_control } else { true };
    let detect_trailing_space = if any_scan {
        cmd.scan_trailing_space
    } else {
        true
    };
    let detect_final_newline = if any_scan {
        cmd.scan_final_newline
    } else {
        true
    };
    let detect_encoding = if any_scan { cmd.scan_encoding } else { true };

    print_command_summary(
        "normalize",
        &cmd.common,
        &encoding,
        &entries,
        &[
            format!("strip_zero_width={}", cmd.strip_zero_width),
            format!("strip_control={}", cmd.strip_control),
            format!("trim_trailing_space={}", cmd.trim_trailing_space),
            format!("ensure_eol={}", cmd.ensure_eol),
            format!("report_format={}", cmd.report_format),
            format!(
                "convert_encoding={}",
                convert_encoding
                    .as_ref()
                    .map(|(_, l)| l.as_str())
                    .unwrap_or("none")
            ),
        ],
    );
    let norm_opts = normalize::NormalizeOptions {
        strip_zero_width: cmd.strip_zero_width,
        strip_control: cmd.strip_control,
        trim_trailing_space: cmd.trim_trailing_space,
        ensure_eol: cmd.ensure_eol,
        detect_zero_width,
        detect_control,
        detect_trailing_space,
        detect_final_newline,
    };
    let mut apply_all = cmd.common.auto_apply;
    let mut stats = CommandStats::default();
    for entry in &entries {
        if entry.metadata.is_probably_binary {
            println!("skipping {} (suspected binary file)", entry.path.display());
            stats.skipped += 1;
            log_change(
                &cmd.common,
                "normalize",
                &entry.path,
                "skipped",
                "suspected binary file",
                &[],
                Some(status_extra(false, !cmd.common.apply)),
            );
            continue;
        }

        let bytes = std::fs::read(&entry.path)
            .with_context(|| format!("reading {}", entry.path.display()))?;
        let decoded = encoding.decode(&bytes);
        let outcome = normalize::normalize_text(&decoded.text, &norm_opts);
        print_normalize_report(
            &entry.path,
            &outcome.report,
            detect_encoding.then_some(decoded.decision.encoding.name()),
            convert_encoding.as_ref().map(|(enc, _)| enc.name()),
            report_format,
        )?;

        let convert_requested = convert_encoding.is_some();
        let convert_only = outcome.cleaned.is_none() && convert_requested;
        let new_text = if let Some(text) = outcome.cleaned {
            text
        } else if convert_requested {
            decoded.text.clone()
        } else {
            stats.no_op += 1;
            if cmd.common.apply {
                log_change(
                    &cmd.common,
                    "normalize",
                    &entry.path,
                    "no-op",
                    "no change",
                    &[],
                    Some(status_extra(false, false)),
                );
            } else {
                emit_json_diff_event(
                    &cmd.common,
                    "normalize",
                    &entry.path,
                    "no-op",
                    "no change",
                    &[],
                    Some(status_extra(false, true)),
                );
            }
            continue;
        };

        let result = TransformResult { decoded, new_text };
        let mut line_summary = diff::summarize_lines(&result.decoded.text, &result.new_text);
        let line_spans = diff::collect_line_spans(&result.decoded.text, &result.new_text);
        if convert_only && line_spans.is_empty() {
            line_summary = format!(
                "encoding conversion to {}",
                convert_encoding
                    .as_ref()
                    .map(|(_, label)| label.as_str())
                    .unwrap_or("requested encoding")
            );
            println!(
                "(no textual diff) {} will be rewritten using {}",
                entry.path.display(),
                convert_encoding
                    .as_ref()
                    .map(|(_, l)| l.as_str())
                    .unwrap_or("requested encoding")
            );
        } else {
            println!("--- preview: {} ---", entry.path.display());
            diff::display_diff(&result.decoded.text, &result.new_text, &diff_config)?;
        }

        if !cmd.common.apply {
            stats.dry_run += 1;
            println!("dry-run: rerun with --apply to write this change.");
            log_change(
                &cmd.common,
                "normalize",
                &entry.path,
                "dry-run",
                &line_summary,
                &line_spans,
                Some(status_extra(false, true)),
            );
            continue;
        }

        let decision = if apply_all {
            ApprovalDecision::Apply
        } else {
            prompt_approval(&entry.path)?
        };

        match decision {
            ApprovalDecision::Apply => {
                apply_transform(
                    entry,
                    &result,
                    convert_encoding.as_ref().map(|(enc, _)| *enc),
                    cmd.common.undo_log.as_deref(),
                    cmd.common.no_backup,
                )?;
                stats.applied += 1;
                log_change(
                    &cmd.common,
                    "normalize",
                    &entry.path,
                    "applied",
                    &line_summary,
                    &line_spans,
                    Some(status_extra(true, false)),
                );
            }
            ApprovalDecision::ApplyAll => {
                apply_all = true;
                apply_transform(
                    entry,
                    &result,
                    convert_encoding.as_ref().map(|(enc, _)| *enc),
                    cmd.common.undo_log.as_deref(),
                    cmd.common.no_backup,
                )?;
                stats.applied += 1;
                log_change(
                    &cmd.common,
                    "normalize",
                    &entry.path,
                    "applied",
                    &line_summary,
                    &line_spans,
                    Some(status_extra(true, false)),
                );
            }
            ApprovalDecision::Skip => {
                println!("skipped {}", entry.path.display());
                stats.skipped += 1;
                log_change(
                    &cmd.common,
                    "normalize",
                    &entry.path,
                    "skipped",
                    &line_summary,
                    &line_spans,
                    Some(status_extra(false, false)),
                );
            }
            ApprovalDecision::Quit => {
                println!("stopping after user request.");
                stats.skipped += 1;
                break;
            }
        }
    }
    stats.print("normalize");
    Ok(())
}

fn handle_script(cmd: ScriptCommand) -> Result<()> {
    let entries = resolve_entries(&cmd.common)?;
    let encoding = resolve_encoding_strategy(&cmd.common)?;
    print_command_summary(
        "script",
        &cmd.common,
        &encoding,
        &entries,
        &[
            format!("script={}", cmd.script.display()),
            format!("args={:?}", cmd.args),
        ],
    );
    Ok(())
}

fn handle_batch(cmd: BatchCommand) -> Result<()> {
    let BatchCommand { common, plan } = cmd;
    let encoding = resolve_encoding_strategy(&common)?;
    let batch_plan = batch::load_plan(&plan)?;
    if batch_plan.steps.is_empty() {
        bail!("plan {} does not contain any steps", plan.display());
    }
    print_command_summary(
        "batch",
        &common,
        &encoding,
        &[],
        &[format!(
            "plan={} ({} steps)",
            plan.display(),
            batch_plan.steps.len()
        )],
    );
    for (idx, step) in batch_plan.steps.iter().enumerate() {
        println!(
            "\n=== Batch Step {}/{}: {} ===",
            idx + 1,
            batch_plan.steps.len(),
            step.kind()
        );
        match step {
            batch::PlanEntry::Replace(step_plan) => {
                let replace_cmd = build_replace_command(&common, step_plan)?;
                handle_replace(replace_cmd)?;
            }
            batch::PlanEntry::Normalize(step_plan) => {
                let normalize_cmd = build_normalize_command(&common, step_plan)?;
                handle_normalize(normalize_cmd)?;
            }
        }
    }
    Ok(())
}

fn handle_log(cmd: LogCommand) -> Result<()> {
    let entries = logging::read_recent(cmd.tail)?;
    if entries.is_empty() {
        println!("change log is empty.");
        return Ok(());
    }
    for entry in entries {
        println!(
            "[{}] {:<10} {:<8} {:<12} {}",
            entry.timestamp, entry.command, entry.action, entry.line_summary, entry.path
        );
        if !entry.spans.is_empty() {
            println!("    spans: {}", describe_spans(&entry.spans));
        }
    }
    Ok(())
}

fn handle_report(cmd: ReportCommand) -> Result<()> {
    let entries = logging::read_all()?;
    if entries.is_empty() {
        println!("change log is empty.");
        return Ok(());
    }
    let since = if let Some(ref raw) = cmd.since {
        let parsed = OffsetDateTime::parse(raw, &Rfc3339)
            .with_context(|| format!("parsing --since '{raw}' as RFC3339 timestamp"))?;
        Some(parsed)
    } else {
        None
    };
    let report_format = ReportFormat::from_str(&cmd.format)?;
    let mut filtered = Vec::new();
    for entry in entries {
        let Ok(ts) = OffsetDateTime::parse(&entry.timestamp, &Rfc3339) else {
            continue;
        };
        if since.is_none_or(|min| ts >= min) {
            filtered.push(entry);
        }
    }
    if filtered.is_empty() {
        println!("no log entries match the requested window.");
        return Ok(());
    }
    let mut summary: BTreeMap<(String, String), usize> = BTreeMap::new();
    for entry in &filtered {
        *summary
            .entry((entry.command.clone(), entry.action.clone()))
            .or_default() += 1;
    }
    match report_format {
        ReportFormat::Table => {
            println!(
                "Report entries: {} (since {})",
                filtered.len(),
                cmd.since.as_deref().unwrap_or("beginning of log")
            );
            for ((command, action), count) in summary {
                println!("{command:<12} {action:<10} {count}");
            }
        }
        ReportFormat::Json => {
            let rows: Vec<_> = summary
                .into_iter()
                .map(|((command, action), count)| {
                    json!({
                        "command": command,
                        "action": action,
                        "count": count
                    })
                })
                .collect();
            println!("{}", serde_json::to_string(&rows)?);
        }
    }
    Ok(())
}

fn handle_cleanup(cmd: CleanupCommand) -> Result<()> {
    let root = fs::canonicalize(&cmd.root)
        .with_context(|| format!("resolving cleanup root {}", cmd.root.display()))?;
    if !root.is_dir() {
        bail!("cleanup root {} is not a directory", root.display());
    }
    let mut candidates = find_backup_files(&root, cmd.include_hidden)?;
    candidates.sort();
    if candidates.is_empty() {
        println!("no .bak files found under {}", root.display());
        return Ok(());
    }
    println!("cleanup root: {}", root.display());
    println!("found {} backup file(s):", candidates.len());
    for path in &candidates {
        println!("  - {}", path.display());
    }
    if !cmd.apply {
        println!("dry-run: rerun with --apply to delete these backups.");
        return Ok(());
    }

    let mut stats = CommandStats::default();
    let mut apply_all = cmd.auto_apply;
    for path in candidates {
        let decision = if apply_all {
            ApprovalDecision::Apply
        } else {
            prompt_approval(&path)?
        };
        match decision {
            ApprovalDecision::Apply => {
                fs::remove_file(&path)
                    .with_context(|| format!("removing backup {}", path.display()))?;
                println!("removed {}", path.display());
                stats.applied += 1;
            }
            ApprovalDecision::ApplyAll => {
                apply_all = true;
                fs::remove_file(&path)
                    .with_context(|| format!("removing backup {}", path.display()))?;
                println!("removed {}", path.display());
                stats.applied += 1;
            }
            ApprovalDecision::Skip => {
                println!("skipped {}", path.display());
                stats.skipped += 1;
            }
            ApprovalDecision::Quit => {
                println!("stopping cleanup after user request.");
                break;
            }
        }
    }
    stats.print("cleanup");
    Ok(())
}

fn describe_spans(spans: &[logging::LineSpan]) -> String {
    spans
        .iter()
        .map(|span| {
            let kind = match span.kind {
                LineSpanKind::Modified => "M",
                LineSpanKind::Added => "A",
            };
            if span.end > span.start {
                format!("{kind} L{}-L{}", span.start, span.end)
            } else {
                format!("{kind} L{}", span.start)
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn find_backup_files(root: &Path, include_hidden: bool) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let walker = WalkDir::new(root).follow_links(false).into_iter();
    for entry in walker.filter_entry(|e| include_hidden || !has_hidden_component(e.path())) {
        let entry = entry?;
        if entry.file_type().is_file() {
            let path = entry.into_path();
            if is_backup_file(&path) {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

fn has_hidden_component(path: &Path) -> bool {
    path.components().any(|component| match component {
        Component::Normal(name) => name
            .to_str()
            .map(|segment| segment.starts_with('.'))
            .unwrap_or(false),
        _ => false,
    })
}

fn is_backup_file(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    if let Some((_base, suffix)) = lower.rsplit_once(".bak") {
        return suffix.chars().all(|ch| ch.is_ascii_digit());
    }
    false
}

fn log_change(
    common: &CommonArgs,
    command: &str,
    path: &Path,
    action: &str,
    line_summary: &str,
    spans: &[LineSpan],
    extra: Option<JsonMap<String, JsonValue>>,
) {
    let _ = record_change(command, path, action, line_summary, spans);
    emit_json_diff_event(common, command, path, action, line_summary, spans, extra);
}

fn emit_json_diff_event(
    common: &CommonArgs,
    command: &str,
    path: &Path,
    action: &str,
    line_summary: &str,
    spans: &[LineSpan],
    extra: Option<JsonMap<String, JsonValue>>,
) {
    if !common.json {
        return;
    }
    let mut event = JsonMap::new();
    event.insert("command".into(), JsonValue::String(command.to_string()));
    event.insert("path".into(), JsonValue::String(path.display().to_string()));
    event.insert("action".into(), JsonValue::String(action.to_string()));
    event.insert(
        "line_summary".into(),
        JsonValue::String(line_summary.to_string()),
    );
    event.insert("spans".into(), spans_to_json(spans));
    if let Some(extra_map) = extra {
        for (key, value) in extra_map {
            event.insert(key, value);
        }
    }
    println!("{}", JsonValue::Object(event));
}

fn spans_to_json(spans: &[LineSpan]) -> JsonValue {
    JsonValue::Array(
        spans
            .iter()
            .map(|span| {
                json!({
                    "kind": match span.kind {
                        LineSpanKind::Modified => "modified",
                        LineSpanKind::Added => "added",
                    },
                    "start": span.start,
                    "end": span.end
                })
            })
            .collect(),
    )
}

fn status_extra(applied: bool, dry_run: bool) -> JsonMap<String, JsonValue> {
    let mut map = JsonMap::new();
    map.insert("applied".into(), JsonValue::Bool(applied));
    map.insert("dry_run".into(), JsonValue::Bool(dry_run));
    map
}

fn status_with_patch(applied: bool, dry_run: bool, kind: PatchKind) -> JsonMap<String, JsonValue> {
    let mut map = status_extra(applied, dry_run);
    map.insert(
        "patch_kind".into(),
        JsonValue::String(patch_kind_label(kind).to_string()),
    );
    map
}

fn patch_kind_label(kind: PatchKind) -> &'static str {
    match kind {
        PatchKind::Modify => "modify",
        PatchKind::Create => "create",
        PatchKind::Delete => "delete",
        PatchKind::Rename => "rename",
    }
}

fn print_command_summary(
    command: &str,
    common: &CommonArgs,
    encoding: &EncodingStrategy,
    entries: &[FileEntry],
    details: &[String],
) {
    println!("command: {command}");
    println!(
        "mode: {}{}",
        if common.apply { "apply" } else { "dry-run" },
        if common.auto_apply {
            " (auto-approve)"
        } else {
            ""
        }
    );
    if !common.targets.is_empty() {
        println!("targets:");
        for target in &common.targets {
            println!("  - {}", target.display());
        }
    } else {
        println!("targets: (none)");
    }
    println!("encoding strategy: {}", encoding.describe());
    println!("context lines: {}", common.context);
    println!("pager: {:?}", common.pager);
    println!("json output: {}", common.json);
    println!("include hidden: {}", common.include_hidden);
    if !common.exclude.is_empty() {
        println!("exclude globs: {:?}", common.exclude);
    }
    if common.no_backup {
        println!("backups disabled");
    }
    if let Some(log) = &common.undo_log {
        println!("undo log dir: {}", log.display());
    }
    if !common.globs.is_empty() {
        println!("globs:");
        for glob in &common.globs {
            println!("  - {glob}");
        }
    }

    if entries.is_empty() {
        println!("resolved files: (none)");
    } else {
        println!("resolved files ({}):", entries.len());
        for entry in entries.iter().take(10) {
            let binary_hint = if entry.metadata.is_probably_binary {
                ", binary? yes"
            } else {
                ""
            };
            println!(
                "  - {} ({} bytes{})",
                entry.path.display(),
                entry.metadata.len,
                binary_hint
            );
        }
        if entries.len() > 10 {
            println!("  ...");
        }
    }
    if !common.extra_args.is_empty() {
        println!("extra args: {:?}", common.extra_args);
    }
    for detail in details {
        println!("{detail}");
    }
    println!("---");
}

fn resolve_entries(common: &CommonArgs) -> Result<Vec<FileEntry>> {
    files::resolve_targets(
        &common.targets,
        &common.globs,
        common.include_hidden,
        &common.exclude,
    )
}

fn resolve_encoding_strategy(common: &CommonArgs) -> Result<EncodingStrategy> {
    EncodingStrategy::new(common.encoding.as_deref())
}

#[derive(Debug, Clone, Copy)]
enum ApprovalDecision {
    Apply,
    Skip,
    ApplyAll,
    Quit,
}

fn prompt_approval(path: &Path) -> Result<ApprovalDecision> {
    loop {
        print_prompt(&format!(
            "Apply change to {}? [y]es/[n]o/[a]ll/[q]uit: ",
            path.display()
        ))?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        match input.trim().to_lowercase().as_str() {
            "y" | "yes" | "" => return Ok(ApprovalDecision::Apply),
            "n" | "no" => return Ok(ApprovalDecision::Skip),
            "a" | "all" => return Ok(ApprovalDecision::ApplyAll),
            "q" | "quit" => return Ok(ApprovalDecision::Quit),
            _ => {
                println!("Please enter y, n, a, or q.");
            }
        }
    }
}

fn print_prompt(message: &str) -> Result<()> {
    print!("{message}");
    io::stdout().flush()?;
    Ok(())
}

#[derive(Clone, Copy)]
enum ReportFormat {
    Table,
    Json,
}

impl ReportFormat {
    fn from_str(value: &str) -> Result<Self> {
        match value.to_lowercase().as_str() {
            "table" => Ok(Self::Table),
            "json" => Ok(Self::Json),
            other => Err(anyhow!(
                "unsupported report-format '{other}' (expected table or json)"
            )),
        }
    }
}

fn print_normalize_report(
    path: &Path,
    report: &normalize::NormalizeReport,
    encoding_name: Option<&str>,
    convert_encoding: Option<&str>,
    format: ReportFormat,
) -> Result<()> {
    match format {
        ReportFormat::Table => {
            println!(
                "{} -> zero-width: {}, control: {}, trailing spaces: {}, missing final newline: {}",
                path.display(),
                format_detection(report.zero_width),
                format_detection(report.control_chars),
                format_detection(report.trailing_spaces),
                format_bool(report.missing_final_newline)
            );
            match (encoding_name, convert_encoding) {
                (Some(src), Some(dst)) => println!("    encoding: {src} -> {dst}"),
                (Some(src), None) => println!("    encoding: {src}"),
                (None, Some(dst)) => println!("    convert encoding: {dst}"),
                _ => {}
            }
        }
        ReportFormat::Json => {
            let row = NormalizeJsonRow {
                path: path.display().to_string(),
                zero_width: report.zero_width,
                control_chars: report.control_chars,
                trailing_spaces: report.trailing_spaces,
                missing_final_newline: report.missing_final_newline,
                encoding: encoding_name.map(|s| s.to_string()),
                convert_encoding: convert_encoding.map(|s| s.to_string()),
            };
            println!("{}", serde_json::to_string(&row)?);
        }
    }
    Ok(())
}

fn format_detection(value: Option<usize>) -> String {
    match value {
        Some(count) => count.to_string(),
        None => "n/a".into(),
    }
}

fn format_bool(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "yes",
        Some(false) => "no",
        None => "n/a",
    }
}

#[derive(Serialize)]
struct NormalizeJsonRow {
    path: String,
    zero_width: Option<usize>,
    control_chars: Option<usize>,
    trailing_spaces: Option<usize>,
    missing_final_newline: Option<bool>,
    encoding: Option<String>,
    convert_encoding: Option<String>,
}

fn apply_transform(
    entry: &FileEntry,
    result: &TransformResult,
    target_encoding: Option<&'static Encoding>,
    undo_dir: Option<&Path>,
    no_backup: bool,
) -> Result<()> {
    if let Some(dir) = undo_dir {
        write_undo_patch(dir, entry, &result.decoded.text, &result.new_text)?;
    }
    let encoding = target_encoding.unwrap_or(result.decoded.decision.encoding);
    let (encoded, _, had_errors) = encoding.encode(&result.new_text);
    if had_errors {
        println!(
            "warning: encoding fallback occurred when writing {}; output may be lossy",
            entry.path.display()
        );
    }
    if let Some(parent) = entry.path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating directory {}", parent.display()))?;
        }
    }
    let backup = create_backup_if_needed(&entry.path, no_backup)?;
    write_via_temp(&entry.path, encoded.as_ref())
        .with_context(|| format!("writing {}", entry.path.display()))?;
    if let Some(bak) = backup {
        println!(
            "backup saved: {} -> {}",
            entry.path.display(),
            bak.display()
        );
    }
    println!("applied {}", entry.path.display());
    Ok(())
}

fn create_backup_if_needed(path: &Path, no_backup: bool) -> Result<Option<PathBuf>> {
    if no_backup || !path.exists() {
        return Ok(None);
    }

    let mut attempt = 0usize;
    loop {
        let candidate = backup_candidate(path, attempt);
        if !candidate.exists() {
            fs::copy(path, &candidate)
                .with_context(|| format!("creating backup {}", candidate.display()))?;
            return Ok(Some(candidate));
        }
        attempt += 1;
    }
}

fn backup_candidate(path: &Path, index: usize) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("safeedit_file");
    let suffix = if index == 0 {
        ".bak".to_string()
    } else {
        format!(".bak{index}")
    };
    let backup_name = format!("{name}{suffix}");
    path.with_file_name(backup_name)
}

fn write_via_temp(path: &Path, data: &[u8]) -> Result<()> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    if let Some(dir) = parent {
        fs::create_dir_all(dir).with_context(|| format!("creating directory {}", dir.display()))?;
    }
    let base_dir = parent.unwrap_or_else(|| Path::new("."));
    let unique = format!(
        ".safeedit-tmp-{}-{}",
        std::process::id(),
        OffsetDateTime::now_utc().unix_timestamp_nanos()
    );
    let temp_path = base_dir.join(unique);
    {
        let mut file = fs::File::create(&temp_path)
            .with_context(|| format!("creating temp file {}", temp_path.display()))?;
        file.write_all(data)
            .with_context(|| format!("writing temp file {}", temp_path.display()))?;
        file.sync_all()
            .with_context(|| format!("syncing temp file {}", temp_path.display()))?;
    }
    fs::rename(&temp_path, path).or_else(|err| {
        let _ = fs::remove_file(&temp_path);
        Err(err).with_context(|| format!("replacing {}", path.display()))
    })?;
    Ok(())
}

fn write_new_file(
    path: &Path,
    new_text: &str,
    encoding: &EncodingStrategy,
    undo_dir: Option<&Path>,
    no_backup: bool,
) -> Result<()> {
    let decision = encoding.decide(b"");
    let decoded = DecodedText {
        text: String::new(),
        had_errors: false,
        decision: decision.clone(),
    };
    let entry = FileEntry {
        path: path.to_path_buf(),
        metadata: FileMetadata {
            len: 0,
            is_probably_binary: false,
        },
    };
    let result = TransformResult {
        decoded,
        new_text: new_text.to_string(),
    };
    apply_transform(
        &entry,
        &result,
        Some(decision.encoding),
        undo_dir,
        no_backup,
    )
}

fn delete_file_with_undo(
    path: &Path,
    old_text: &str,
    undo_dir: Option<&Path>,
    no_backup: bool,
) -> Result<()> {
    let entry = FileEntry {
        path: path.to_path_buf(),
        metadata: FileMetadata {
            len: old_text.len() as u64,
            is_probably_binary: false,
        },
    };
    if let Some(dir) = undo_dir {
        write_undo_patch(dir, &entry, old_text, "")?;
    }
    if path.exists() {
        let backup = create_backup_if_needed(path, no_backup)?;
        if let Some(bak) = backup {
            println!("backup saved: {} -> {}", path.display(), bak.display());
        }
        fs::remove_file(path).with_context(|| format!("removing {}", path.display()))?;
    }
    println!("deleted {}", path.display());
    Ok(())
}

fn write_undo_patch(dir: &Path, entry: &FileEntry, old_text: &str, new_text: &str) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("creating undo dir {}", dir.display()))?;
    let timestamp = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "unknown".into());
    let sanitized = sanitize_path(&entry.path);
    let file_name = format!("{timestamp}_{sanitized}.patch");
    let patch_path = dir.join(file_name);
    let diff = diff::unified_diff(&entry.path, &entry.path, new_text, old_text, 3);
    fs::write(&patch_path, diff)
        .with_context(|| format!("writing undo patch {}", patch_path.display()))?;
    Ok(())
}

fn sanitize_path(path: &Path) -> String {
    path.display()
        .to_string()
        .chars()
        .map(|ch| match ch {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => ch,
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LineEndingStyle {
    Lf,
    Crlf,
    Cr,
}

fn detect_line_ending_style(text: &str) -> LineEndingStyle {
    if text.contains("\r\n") {
        LineEndingStyle::Crlf
    } else if text.contains('\r') {
        LineEndingStyle::Cr
    } else {
        LineEndingStyle::Lf
    }
}

fn normalize_to_lf(text: &str) -> Cow<'_, str> {
    if !text.contains('\r') {
        return Cow::Borrowed(text);
    }
    let mut normalized = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\r' => {
                if matches!(chars.peek(), Some('\n')) {
                    chars.next();
                }
                normalized.push('\n');
            }
            _ => normalized.push(ch),
        }
    }
    Cow::Owned(normalized)
}

fn restore_from_lf(text: String, style: LineEndingStyle) -> String {
    match style {
        LineEndingStyle::Lf => text,
        LineEndingStyle::Crlf => text.replace('\n', "\r\n"),
        LineEndingStyle::Cr => text.replace('\n', "\r"),
    }
}

fn apply_patch_preserving_newlines<'a>(
    text: &str,
    parsed_patch: &DiffPatch<'a, str>,
) -> std::result::Result<String, diffy::ApplyError> {
    let style = detect_line_ending_style(text);
    let normalized = normalize_to_lf(text);
    let patched = apply_patch(normalized.as_ref(), parsed_patch)?;
    Ok(restore_from_lf(patched, style))
}

#[derive(Default)]
struct CommandStats {
    applied: usize,
    skipped: usize,
    dry_run: usize,
    no_op: usize,
}

impl CommandStats {
    fn print(&self, label: &str) {
        let total = self.applied + self.skipped + self.dry_run + self.no_op;
        if total == 0 {
            return;
        }
        println!(
            "{label} summary: applied={}, skipped={}, dry-run={}, no-op={}",
            self.applied, self.skipped, self.dry_run, self.no_op
        );
    }
}

fn merge_common(base: &CommonArgs, overrides: &batch::PlanCommon) -> CommonArgs {
    let mut merged = base.clone();
    if let Some(targets) = &overrides.targets {
        merged.targets = targets.clone();
    }
    if let Some(globs) = &overrides.globs {
        merged.globs = globs.clone();
    }
    if let Some(encoding) = &overrides.encoding {
        merged.encoding = Some(encoding.clone());
    }
    if let Some(apply) = overrides.apply {
        merged.apply = apply;
    }
    if let Some(auto) = overrides.auto_apply {
        merged.auto_apply = auto;
    }
    if let Some(no_backup) = overrides.no_backup {
        merged.no_backup = no_backup;
    }
    if let Some(context) = overrides.context {
        merged.context = context;
    }
    if let Some(pager) = overrides.pager {
        merged.pager = pager;
    }
    if let Some(color) = overrides.color {
        merged.color = color;
    }
    if let Some(json) = overrides.json {
        merged.json = json;
    }
    if let Some(include_hidden) = overrides.include_hidden {
        merged.include_hidden = include_hidden;
    }
    if let Some(exclude) = &overrides.exclude {
        merged.exclude = exclude.clone();
    }
    if let Some(undo_log) = &overrides.undo_log {
        merged.undo_log = Some(undo_log.clone());
    }
    merged
}

fn build_replace_command(
    base_common: &CommonArgs,
    step: &batch::ReplacePlan,
) -> Result<ReplaceCommand> {
    if step.replacement.is_none() && !step.with_stdin && !step.with_clipboard {
        bail!("replace step missing replacement text or input source");
    }
    Ok(ReplaceCommand {
        common: merge_common(base_common, &step.common),
        pattern: step.pattern.clone(),
        replacement: step.replacement.clone(),
        with_stdin: step.with_stdin,
        with_clipboard: step.with_clipboard,
        with_here: None,
        regex: step.regex,
        literal: step.literal,
        diff_only: step.diff_only,
        count: step.count,
        expect: step.expect,
        after_line: step.after_line,
    })
}

fn build_normalize_command(
    base_common: &CommonArgs,
    step: &batch::NormalizePlan,
) -> Result<NormalizeCommand> {
    Ok(NormalizeCommand {
        common: merge_common(base_common, &step.common),
        convert_encoding: step.convert_encoding.clone(),
        strip_zero_width: step.strip_zero_width.unwrap_or(false),
        strip_control: step.strip_control.unwrap_or(false),
        trim_trailing_space: step.trim_trailing_space.unwrap_or(false),
        ensure_eol: step.ensure_eol.unwrap_or(false),
        report_format: step
            .report_format
            .clone()
            .unwrap_or_else(|| "table".to_string()),
        scan_encoding: step.scan_encoding.unwrap_or(false),
        scan_zero_width: step.scan_zero_width.unwrap_or(false),
        scan_control: step.scan_control.unwrap_or(false),
        scan_trailing_space: step.scan_trailing_space.unwrap_or(false),
        scan_final_newline: step.scan_final_newline.unwrap_or(false),
    })
}

#[derive(Debug, Parser)]
#[command(name = "safeedit", version, about = "Safe file editing companion")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Replace(ReplaceCommand),
    Apply(ApplyCommand),
    Block(BlockCommand),
    Rename(RenameCommand),
    Review(ReviewCommand),
    Normalize(NormalizeCommand),
    Script(ScriptCommand),
    Batch(BatchCommand),
    Log(LogCommand),
    Report(ReportCommand),
    Cleanup(CleanupCommand),
    Write(WriteCommand),
}

#[derive(Debug, Clone, Args)]
struct CommonArgs {
    #[arg(long = "glob", value_name = "GLOB")]
    globs: Vec<String>,
    #[arg(long = "target", value_name = "PATH", value_hint = ValueHint::AnyPath)]
    targets: Vec<PathBuf>,
    #[arg(long, value_name = "ENCODING")]
    encoding: Option<String>,
    #[arg(long, action = ArgAction::SetTrue)]
    apply: bool,
    #[arg(long = "yes", action = ArgAction::SetTrue)]
    auto_apply: bool,
    #[arg(long, action = ArgAction::SetTrue)]
    no_backup: bool,
    #[arg(long, default_value_t = 3)]
    context: usize,
    #[arg(long = "pager", value_enum, default_value = "auto")]
    pager: PagerMode,
    #[arg(long = "color", value_enum, default_value = "auto")]
    color: ColorChoice,
    #[arg(long, action = ArgAction::SetTrue)]
    json: bool,
    #[arg(long = "include-hidden", action = ArgAction::SetTrue)]
    include_hidden: bool,
    #[arg(long = "exclude", value_name = "GLOB")]
    exclude: Vec<String>,
    #[arg(long = "undo-log", value_name = "DIR", value_hint = ValueHint::DirPath)]
    undo_log: Option<PathBuf>,
    #[arg(value_name = "EXTRA", value_parser = value_parser!(String))]
    extra_args: Vec<String>,
}

impl CommonArgs {
    fn diff_display_config(&self, colorize: bool) -> diff::DiffDisplayConfig {
        diff::DiffDisplayConfig {
            context: self.context,
            colorize,
            pager_mode: self.pager,
            interactive: self.allow_interactive_pager(),
        }
    }

    fn allow_interactive_pager(&self) -> bool {
        io::stdin().is_terminal() && io::stdout().is_terminal() && !self.auto_apply && !self.json
    }
}

#[derive(Debug, Args)]
struct ReplaceCommand {
    #[command(flatten)]
    common: CommonArgs,
    #[arg(long, value_name = "PATTERN")]
    pattern: String,
    #[arg(
        long = "with",
        value_name = "TEXT",
        conflicts_with_all = ["with_stdin", "with_clipboard"],
        required_unless_present_any = ["with_stdin", "with_clipboard", "with_here"]
    )]
    replacement: Option<String>,
    #[arg(long = "with-stdin", action = ArgAction::SetTrue, conflicts_with = "with_clipboard")]
    with_stdin: bool,
    #[arg(long = "with-clipboard", action = ArgAction::SetTrue, conflicts_with = "with_stdin")]
    with_clipboard: bool,
    #[arg(long = "with-here", value_name = "TAG", conflicts_with_all = ["replacement", "with_stdin", "with_clipboard"])]
    with_here: Option<String>,
    #[arg(long, action = ArgAction::SetTrue)]
    regex: bool,
    #[arg(long, action = ArgAction::SetTrue)]
    literal: bool,
    #[arg(long = "diff-only", action = ArgAction::SetTrue)]
    diff_only: bool,
    #[arg(long, value_name = "N")]
    count: Option<usize>,
    #[arg(long, value_name = "N")]
    expect: Option<usize>,
    #[arg(long = "after-line", value_name = "LINE")]
    after_line: Option<usize>,
}

#[derive(Debug, Args)]
struct ApplyCommand {
    #[command(flatten)]
    common: CommonArgs,
    #[arg(
        long = "patch",
        value_name = "FILE",
        value_hint = ValueHint::FilePath,
        required = true,
        action = ArgAction::Append
    )]
    patch_files: Vec<PathBuf>,
    #[arg(long = "root", value_name = "DIR", value_hint = ValueHint::DirPath)]
    root: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct BlockCommand {
    #[command(flatten)]
    common: CommonArgs,
    #[arg(long = "start-marker", value_name = "TEXT")]
    start_marker: String,
    #[arg(long = "end-marker", value_name = "TEXT")]
    end_marker: String,
    #[arg(long, value_name = "MODE", default_value = "replace")]
    mode: BlockMode,
    #[arg(
        long = "body",
        value_name = "TEXT",
        action = ArgAction::Append,
        conflicts_with_all = ["body_file", "with_stdin", "with_clipboard", "body_here"],
        required_unless_present_any = ["body_file", "with_stdin", "with_clipboard", "body_here"]
    )]
    body: Vec<String>,
    #[arg(
        long = "body-file",
        value_name = "FILE",
        value_hint = ValueHint::FilePath,
        conflicts_with_all = ["body", "with_stdin", "with_clipboard", "body_here"]
    )]
    body_file: Option<PathBuf>,
    #[arg(long = "with-stdin", action = ArgAction::SetTrue, conflicts_with_all = ["body", "body_file", "with_clipboard", "body_here"])]
    with_stdin: bool,
    #[arg(long = "with-clipboard", action = ArgAction::SetTrue, conflicts_with_all = ["body", "body_file", "with_stdin", "body_here"])]
    with_clipboard: bool,
    #[arg(long = "body-here", value_name = "TAG", conflicts_with_all = ["body", "body_file", "with_stdin", "with_clipboard"])]
    body_here: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockMode {
    Insert,
    Replace,
}

impl std::str::FromStr for BlockMode {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "insert" => Ok(Self::Insert),
            "replace" => Ok(Self::Replace),
            _ => Err("expected 'insert' or 'replace'"),
        }
    }
}

#[derive(Debug, Args)]
struct WriteCommand {
    #[command(flatten)]
    common: CommonArgs,
    #[arg(long = "path", value_name = "FILE", value_hint = ValueHint::FilePath)]
    path: PathBuf,
    #[arg(
        long = "body",
        value_name = "TEXT",
        action = ArgAction::Append,
        conflicts_with_all = ["body_file", "with_stdin", "with_clipboard", "body_here"],
        required_unless_present_any = ["body_file", "with_stdin", "with_clipboard", "body_here"]
    )]
    body: Vec<String>,
    #[arg(
        long = "body-file",
        value_name = "FILE",
        value_hint = ValueHint::FilePath,
        conflicts_with_all = ["body", "with_stdin", "with_clipboard", "body_here"]
    )]
    body_file: Option<PathBuf>,
    #[arg(
        long = "with-stdin",
        action = ArgAction::SetTrue,
        conflicts_with_all = ["body", "body_file", "with_clipboard", "body_here"]
    )]
    with_stdin: bool,
    #[arg(
        long = "with-clipboard",
        action = ArgAction::SetTrue,
        conflicts_with_all = ["body", "body_file", "with_stdin", "body_here"]
    )]
    with_clipboard: bool,
    #[arg(
        long = "body-here",
        value_name = "TAG",
        conflicts_with_all = ["body", "body_file", "with_stdin", "with_clipboard"]
    )]
    body_here: Option<String>,
    #[arg(long = "allow-overwrite", action = ArgAction::SetTrue)]
    allow_overwrite: bool,
    #[arg(long = "line-ending", value_enum, default_value = "auto")]
    line_ending: LineEndingChoice,
}

#[derive(Debug, Args)]
struct RenameCommand {
    #[command(flatten)]
    common: CommonArgs,
    #[arg(long, value_name = "OLD")]
    from: String,
    #[arg(long, value_name = "NEW")]
    to: String,
    #[arg(long = "word-boundary", action = ArgAction::SetTrue)]
    word_boundary: bool,
    #[arg(long = "case-aware", action = ArgAction::SetTrue)]
    case_aware: bool,
}

#[derive(Debug, Args)]
struct ReviewCommand {
    #[command(flatten)]
    common: CommonArgs,
    #[arg(long, value_name = "N")]
    head: Option<usize>,
    #[arg(long, value_name = "N")]
    tail: Option<usize>,
    #[arg(long, value_name = "START:END")]
    lines: Option<String>,
    #[arg(long, value_name = "LINE:CONTEXT")]
    around: Option<String>,
    #[arg(long, action = ArgAction::SetTrue)]
    follow: bool,
    #[arg(long, value_name = "PATTERN")]
    search: Option<String>,
    #[arg(long, action = ArgAction::SetTrue)]
    regex: bool,
    #[arg(long, action = ArgAction::SetTrue)]
    step: bool,
}

#[derive(Debug, Args)]
struct NormalizeCommand {
    #[command(flatten)]
    common: CommonArgs,
    #[arg(long = "convert-encoding", value_name = "ENCODING")]
    convert_encoding: Option<String>,
    #[arg(long = "strip-zero-width", action = ArgAction::SetTrue)]
    strip_zero_width: bool,
    #[arg(long = "strip-control", action = ArgAction::SetTrue)]
    strip_control: bool,
    #[arg(long = "trim-trailing-space", action = ArgAction::SetTrue)]
    trim_trailing_space: bool,
    #[arg(long = "ensure-eol", action = ArgAction::SetTrue)]
    ensure_eol: bool,
    #[arg(long = "report-format", default_value = "table")]
    report_format: String,
    #[arg(long = "scan-encoding", action = ArgAction::SetTrue)]
    scan_encoding: bool,
    #[arg(long = "scan-zero-width", action = ArgAction::SetTrue)]
    scan_zero_width: bool,
    #[arg(long = "scan-control", action = ArgAction::SetTrue)]
    scan_control: bool,
    #[arg(long = "scan-trailing-space", action = ArgAction::SetTrue)]
    scan_trailing_space: bool,
    #[arg(long = "scan-final-newline", action = ArgAction::SetTrue)]
    scan_final_newline: bool,
}

#[derive(Debug, Args)]
struct ScriptCommand {
    #[command(flatten)]
    common: CommonArgs,
    #[arg(value_name = "SCRIPT", value_hint = ValueHint::FilePath)]
    script: PathBuf,
    #[arg(long = "arg", value_name = "VALUE")]
    args: Vec<String>,
}

#[derive(Debug, Args)]
struct BatchCommand {
    #[command(flatten)]
    common: CommonArgs,
    #[arg(value_name = "PLAN", value_hint = ValueHint::FilePath)]
    plan: PathBuf,
}

#[derive(Debug, Args)]
struct LogCommand {
    #[arg(long = "tail", default_value_t = 20)]
    tail: usize,
}

#[derive(Debug, Args)]
struct ReportCommand {
    #[arg(long = "since", value_name = "RFC3339")]
    since: Option<String>,
    #[arg(long = "format", default_value = "table")]
    format: String,
}

#[cfg(test)]
mod patch_line_ending_tests {
    use super::{DiffPatch, LineEndingChoice, LineEndingStyle, apply_patch_preserving_newlines};

    #[test]
    fn apply_patch_respects_crlf_inputs() {
        let original = "alpha\r\nbeta\r\n";
        let patch_text = "\
--- a/file.txt
+++ b/file.txt
@@ -1,2 +1,2 @@
 alpha
-beta
+beta2
";
        let patch = DiffPatch::from_str(patch_text).expect("patch parses");
        let patched =
            apply_patch_preserving_newlines(original, &patch).expect("patch applies cleanly");
        assert_eq!(patched, "alpha\r\nbeta2\r\n");
    }
    #[test]
    fn line_ending_choice_prefers_existing_style() {
        assert_eq!(
            LineEndingChoice::Auto.resolve(Some(LineEndingStyle::Cr)),
            LineEndingStyle::Cr
        );
    }

    #[test]
    fn line_ending_choice_specific_variants() {
        assert_eq!(
            LineEndingChoice::Lf.resolve(Some(LineEndingStyle::Crlf)),
            LineEndingStyle::Lf
        );
        assert_eq!(
            LineEndingChoice::Crlf.resolve(Some(LineEndingStyle::Lf)),
            LineEndingStyle::Crlf
        );
        assert_eq!(
            LineEndingChoice::Cr.resolve(Some(LineEndingStyle::Lf)),
            LineEndingStyle::Cr
        );
    }

    #[test]
    fn line_ending_choice_auto_uses_platform_default() {
        let expected = if cfg!(windows) {
            LineEndingStyle::Crlf
        } else {
            LineEndingStyle::Lf
        };
        assert_eq!(LineEndingChoice::Auto.resolve(None), expected);
    }
}

#[cfg(test)]
mod cleanup_tests {
    use super::is_backup_file;
    use std::path::Path;

    #[test]
    fn backup_detector_flags_basic_suffix() {
        assert!(is_backup_file(Path::new("foo.txt.bak")));
    }

    #[test]
    fn backup_detector_flags_incremental_suffix() {
        assert!(is_backup_file(Path::new("docs/plan.md.bak12")));
    }

    #[test]
    fn backup_detector_ignores_other_files() {
        assert!(!is_backup_file(Path::new("notes/backup-plan.md")));
        assert!(!is_backup_file(Path::new("README.md")));
        assert!(!is_backup_file(Path::new("file.bakup")));
    }
}

#[derive(Debug, Args)]
struct CleanupCommand {
    #[arg(long = "root", value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
    root: PathBuf,
    #[arg(long, action = ArgAction::SetTrue)]
    apply: bool,
    #[arg(long = "yes", action = ArgAction::SetTrue)]
    auto_apply: bool,
    #[arg(long = "include-hidden", action = ArgAction::SetTrue)]
    include_hidden: bool,
}
