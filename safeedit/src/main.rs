use std::fs;
use std::io::{self, Write as IoWrite};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{ArgAction, Args, Parser, Subcommand, ValueHint, value_parser};

mod commands;
mod diff;
mod encoding;
mod files;
mod review;
mod transform;
use commands::{ReplaceOptions, run_replace};
use encoding::EncodingStrategy;
use files::FileEntry;
use transform::TransformResult;

fn main() -> Result<()> {
    let cli = Cli::parse();
    run(cli)
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Replace(cmd) => handle_replace(cmd)?,
        Command::Block(cmd) => handle_block(cmd)?,
        Command::Rename(cmd) => handle_rename(cmd)?,
        Command::Review(cmd) => handle_review(cmd)?,
        Command::Normalize(cmd) => handle_normalize(cmd)?,
        Command::Script(cmd) => handle_script(cmd)?,
        Command::Batch(cmd) => handle_batch(cmd)?,
    }

    Ok(())
}

fn handle_replace(cmd: ReplaceCommand) -> Result<()> {
    let entries = resolve_entries(&cmd.common)?;
    let encoding = resolve_encoding_strategy(&cmd.common)?;
    let literal_mode = cmd.literal || !cmd.regex;
    let pattern = if literal_mode {
        regex::escape(&cmd.pattern)
    } else {
        cmd.pattern.clone()
    };
    let replace_options = ReplaceOptions {
        pattern,
        replacement: cmd.replacement.clone(),
        allow_captures: !literal_mode,
        count: cmd.count,
        expect: cmd.expect,
    };
    print_command_summary(
        "replace",
        &cmd.common,
        &encoding,
        &entries,
        &[
            format!("pattern={}", cmd.pattern),
            format!("replacement={}", cmd.replacement),
            format!("mode={}", if literal_mode { "literal" } else { "regex" }),
            format!("count={:?}", cmd.count),
            format!("expect={:?}", cmd.expect),
            format!("after_line={:?}", cmd.after_line),
        ],
    );
    let mut apply_all = false;
    for entry in &entries {
        let Some(result) = run_replace(entry, &encoding, &replace_options)? else {
            continue;
        };

        println!("--- preview: {} ---", entry.path.display());
        diff::print_diff(&result.decoded.text, &result.new_text, cmd.common.context)?;

        if !cmd.common.apply {
            println!("dry-run: rerun with --apply to write this change.");
            continue;
        }

        let decision = if apply_all {
            ApprovalDecision::Apply
        } else {
            prompt_approval(&entry.path)?
        };

        match decision {
            ApprovalDecision::Apply => apply_transform(entry, &result)?,
            ApprovalDecision::ApplyAll => {
                apply_all = true;
                apply_transform(entry, &result)?;
            }
            ApprovalDecision::Skip => {
                println!("skipped {}", entry.path.display());
            }
            ApprovalDecision::Quit => {
                println!("stopping after user request.");
                break;
            }
        }
    }
    Ok(())
}

fn handle_block(cmd: BlockCommand) -> Result<()> {
    let entries = resolve_entries(&cmd.common)?;
    let encoding = resolve_encoding_strategy(&cmd.common)?;
    print_command_summary(
        "block",
        &cmd.common,
        &encoding,
        &entries,
        &[
            format!("start_marker={}", cmd.start_marker),
            format!("end_marker={}", cmd.end_marker),
            format!("mode={:?}", cmd.mode),
        ],
    );
    Ok(())
}

fn handle_rename(cmd: RenameCommand) -> Result<()> {
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
        search: cmd.search.as_deref(),
        regex: cmd.regex,
    })?;
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
            format!("search={:?}", cmd.search),
            format!("regex={}", cmd.regex),
        ],
    );
    review::run(&entries, &encoding, &review_options)?;
    Ok(())
}

fn handle_normalize(cmd: NormalizeCommand) -> Result<()> {
    let entries = resolve_entries(&cmd.common)?;
    let encoding = resolve_encoding_strategy(&cmd.common)?;
    print_command_summary(
        "normalize",
        &cmd.common,
        &encoding,
        &entries,
        &[
            format!("convert_encoding={:?}", cmd.convert_encoding),
            format!("strip_zero_width={}", cmd.strip_zero_width),
            format!("strip_control={}", cmd.strip_control),
            format!("trim_trailing_space={}", cmd.trim_trailing_space),
            format!("ensure_eol={}", cmd.ensure_eol),
            format!("report_format={}", cmd.report_format),
            format!("scan_encoding={}", cmd.scan_encoding),
            format!("scan_zero_width={}", cmd.scan_zero_width),
            format!("scan_control={}", cmd.scan_control),
            format!("scan_trailing_space={}", cmd.scan_trailing_space),
        ],
    );
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
    let encoding = resolve_encoding_strategy(&cmd.common)?;
    print_command_summary(
        "batch",
        &cmd.common,
        &encoding,
        &[],
        &[format!("plan={}", cmd.plan.display())],
    );
    Ok(())
}

fn print_command_summary(
    command: &str,
    common: &CommonArgs,
    encoding: &EncodingStrategy,
    entries: &[FileEntry],
    details: &[String],
) {
    println!("command: {command}");
    println!("mode: {}", if common.apply { "apply" } else { "dry-run" });
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

fn apply_transform(entry: &FileEntry, result: &TransformResult) -> Result<()> {
    let (encoded, _, had_errors) = result.decoded.decision.encoding.encode(&result.new_text);
    if had_errors {
        println!(
            "warning: encoding fallback occurred when writing {}; output may be lossy",
            entry.path.display()
        );
    }
    fs::write(&entry.path, encoded.as_ref())
        .with_context(|| format!("writing {}", entry.path.display()))?;
    println!("applied {}", entry.path.display());
    Ok(())
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
    Block(BlockCommand),
    Rename(RenameCommand),
    Review(ReviewCommand),
    Normalize(NormalizeCommand),
    Script(ScriptCommand),
    Batch(BatchCommand),
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
    #[arg(long, action = ArgAction::SetTrue)]
    no_backup: bool,
    #[arg(long, default_value_t = 3)]
    context: usize,
    #[arg(long, value_name = "PAGER")]
    pager: Option<String>,
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

#[derive(Debug, Args)]
struct ReplaceCommand {
    #[command(flatten)]
    common: CommonArgs,
    #[arg(long, value_name = "PATTERN")]
    pattern: String,
    #[arg(long = "with", value_name = "TEXT")]
    replacement: String,
    #[arg(long, action = ArgAction::SetTrue)]
    regex: bool,
    #[arg(long, action = ArgAction::SetTrue)]
    literal: bool,
    #[arg(long, value_name = "N")]
    count: Option<usize>,
    #[arg(long, value_name = "N")]
    expect: Option<usize>,
    #[arg(long = "after-line", value_name = "LINE")]
    after_line: Option<usize>,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockMode {
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
