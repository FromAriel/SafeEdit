use anyhow::{Context, Result, anyhow, bail};
use regex::{Captures, Regex};
use std::fs;
use std::io::{self, Write};

use crate::encoding::{DecodedText, EncodingStrategy};
use crate::files::FileEntry;

const DEFAULT_HEAD_LINES: usize = 40;

#[derive(Debug, Clone)]
pub struct ReviewInput<'a> {
    pub head: Option<usize>,
    pub tail: Option<usize>,
    pub lines: Option<&'a str>,
    pub around: Option<&'a str>,
    pub follow: bool,
    pub step: bool,
    pub search: Option<&'a str>,
    pub regex: bool,
}

#[derive(Debug, Clone)]
pub struct ReviewOptions {
    slices: Vec<ReviewSlice>,
    matcher: Option<Regex>,
    follow: bool,
    step: bool,
}

#[derive(Debug, Clone)]
enum ReviewSlice {
    Head(usize),
    Tail(usize),
    Range { start: usize, end: usize },
    Around { line: usize, context: usize },
}

impl ReviewOptions {
    pub fn from_input(input: ReviewInput<'_>) -> Result<Self> {
        let mut slices = Vec::new();

        if let Some(range) = input.lines {
            let (start, end) = parse_range_spec(range)?;
            slices.push(ReviewSlice::Range { start, end });
        }

        if let Some(around) = input.around {
            let (line, context) = parse_line_context(around)?;
            slices.push(ReviewSlice::Around { line, context });
        }

        if let Some(head) = input.head {
            slices.push(ReviewSlice::Head(head));
        }

        if let Some(tail) = input.tail {
            slices.push(ReviewSlice::Tail(tail));
        }

        if slices.is_empty() {
            slices.push(ReviewSlice::Head(DEFAULT_HEAD_LINES));
        }

        let matcher = build_matcher(input.search, input.regex)?;

        Ok(Self {
            slices,
            matcher,
            follow: input.follow,
            step: input.step,
        })
    }

    pub fn matcher(&self) -> Option<&Regex> {
        self.matcher.as_ref()
    }

    pub fn step_mode(&self) -> bool {
        self.step
    }
}

pub fn run(
    entries: &[FileEntry],
    encoding: &EncodingStrategy,
    options: &ReviewOptions,
) -> Result<()> {
    if options.follow {
        println!("follow mode is not yet implemented; proceeding with static preview.");
    }

    for entry in entries {
        review_file(entry, encoding, options)?;
    }

    Ok(())
}

fn review_file(
    entry: &FileEntry,
    encoding: &EncodingStrategy,
    options: &ReviewOptions,
) -> Result<()> {
    println!("=== {} ===", entry.path.display());

    if entry.metadata.is_probably_binary {
        println!("skipping (suspected binary file)");
        return Ok(());
    }

    let bytes = fs::read(&entry.path)
        .with_context(|| format!("failed to read {}", entry.path.display()))?;
    let decoded = encoding.decode(&bytes);

    println!(
        "decoded as {} via {} (errors: {})",
        decoded.decision.encoding.name(),
        decoded.decision.source,
        if decoded.had_errors { "yes" } else { "no" }
    );

    if options.step_mode() {
        run_step_mode(&decoded, options.matcher())?;
    } else {
        render_content(&decoded, options)?;
    }
    Ok(())
}

fn render_content(decoded: &DecodedText, options: &ReviewOptions) -> Result<()> {
    let lines: Vec<&str> = decoded.text.lines().collect();

    if lines.is_empty() {
        println!("(file is empty)");
        return Ok(());
    }

    for slice in &options.slices {
        match slice {
            ReviewSlice::Head(count) => {
                println!("-- head ({count} lines) --");
                let end = (*count).min(lines.len());
                print_lines(&lines, 0, end, options.matcher());
            }
            ReviewSlice::Tail(count) => {
                println!("-- tail ({count} lines) --");
                let start = lines.len().saturating_sub(*count);
                print_lines(&lines, start, lines.len(), options.matcher());
            }
            ReviewSlice::Range { start, end } => {
                println!("-- lines {start} to {end} --");
                let (start_idx, end_idx) = to_indices(*start, *end, lines.len());
                print_lines(&lines, start_idx, end_idx, options.matcher());
            }
            ReviewSlice::Around { line, context } => {
                println!("-- around line {line} ± {context} --");
                let start_line = line.saturating_sub(*context);
                let end_line = line + *context;
                let (start_idx, end_idx) = to_indices(start_line, end_line, lines.len());
                print_lines(&lines, start_idx, end_idx, options.matcher());
            }
        }
    }

    Ok(())
}

fn print_lines(lines: &[&str], start_idx: usize, end_idx: usize, matcher: Option<&Regex>) {
    let end_idx = end_idx.min(lines.len());
    for (offset, line) in lines[start_idx..end_idx].iter().enumerate() {
        let number = start_idx + offset + 1;
        let rendered = highlight_line(line, matcher);
        println!("{number:>6} | {rendered}");
    }
}

fn highlight_line(line: &str, matcher: Option<&Regex>) -> String {
    if let Some(regex) = matcher {
        regex
            .replace_all(line, |caps: &Captures<'_>| format!(">>{}<<", &caps[0]))
            .into_owned()
    } else {
        line.to_string()
    }
}

fn run_step_mode(decoded: &DecodedText, matcher: Option<&Regex>) -> Result<()> {
    let lines: Vec<&str> = decoded.text.lines().collect();
    if lines.is_empty() {
        println!("(file is empty)");
        return Ok(());
    }

    println!(
        "Entering step mode. Commands: [Enter]/j=next line, b/p/k=previous line, g/G=head/tail, n/N=next/prev match, /pattern=set search, m=mark, '=jump mark, q=quit, ?=help"
    );

    let mut index = 0usize;
    let mut bookmark: Option<usize> = None;
    let mut dynamic_search: Option<Regex> = None;

    loop {
        print_step_line(
            &lines,
            index,
            active_search(dynamic_search.as_ref(), matcher),
        );
        print!("step> ");
        io::stdout().flush()?;
        let mut input = String::new();
        let bytes = io::stdin()
            .read_line(&mut input)
            .context("reading step input")?;
        if bytes == 0 {
            println!("stdin closed; exiting step mode.");
            break;
        }

        match parse_step_command(input.trim()) {
            StepCommand::NextLine => {
                if index + 1 < lines.len() {
                    index += 1;
                } else {
                    println!("(end of file)");
                }
            }
            StepCommand::PrevLine => {
                if index > 0 {
                    index -= 1;
                } else {
                    println!("(start of file)");
                }
            }
            StepCommand::Head => index = 0,
            StepCommand::Tail => index = lines.len().saturating_sub(1),
            StepCommand::Jump(target) => {
                if target < lines.len() {
                    index = target;
                } else {
                    println!("line {} is out of range (1-{})", target + 1, lines.len());
                }
            }
            StepCommand::Search(pattern) => {
                if pattern.trim().is_empty() {
                    dynamic_search = None;
                    println!("cleared interactive search pattern.");
                } else {
                    match build_interactive_regex(pattern.trim()) {
                        Ok(regex) => {
                            dynamic_search = Some(regex);
                            println!("search set; use 'n'/'N' to jump between matches.");
                        }
                        Err(err) => println!("invalid search pattern: {err}"),
                    }
                }
            }
            StepCommand::FindNext => {
                if let Some(regex) = active_search(dynamic_search.as_ref(), matcher) {
                    if let Some(hit) = find_next_match(&lines, index, regex) {
                        index = hit;
                    } else {
                        println!("no later matches.");
                    }
                } else {
                    println!("no search pattern set. Use --search or type /pattern.");
                }
            }
            StepCommand::FindPrev => {
                if let Some(regex) = active_search(dynamic_search.as_ref(), matcher) {
                    if let Some(hit) = find_prev_match(&lines, index, regex) {
                        index = hit;
                    } else {
                        println!("no earlier matches.");
                    }
                } else {
                    println!("no search pattern set. Use --search or type /pattern.");
                }
            }
            StepCommand::SetBookmark => {
                bookmark = Some(index);
                println!("bookmark set at line {}", index + 1);
            }
            StepCommand::JumpBookmark => {
                if let Some(mark) = bookmark {
                    index = mark;
                } else {
                    println!("no bookmark set. Type 'm' to set one.");
                }
            }
            StepCommand::Help => print_step_help(),
            StepCommand::Quit => break,
        }
    }

    Ok(())
}

fn active_search<'a>(dynamic: Option<&'a Regex>, fallback: Option<&'a Regex>) -> Option<&'a Regex> {
    dynamic.or(fallback)
}

fn find_next_match(lines: &[&str], index: usize, regex: &Regex) -> Option<usize> {
    let mut pos = index + 1;
    while pos < lines.len() {
        if regex.is_match(lines[pos]) {
            return Some(pos);
        }
        pos += 1;
    }
    None
}

fn find_prev_match(lines: &[&str], index: usize, regex: &Regex) -> Option<usize> {
    let mut pos = index;
    while pos > 0 {
        pos -= 1;
        if regex.is_match(lines[pos]) {
            return Some(pos);
        }
    }
    None
}

fn print_step_line(lines: &[&str], index: usize, matcher: Option<&Regex>) {
    if let Some(line) = lines.get(index) {
        let rendered = highlight_line(line, matcher);
        println!("{:>6} | {}", index + 1, rendered);
    }
}

#[derive(Debug, PartialEq, Eq)]
enum StepCommand {
    NextLine,
    PrevLine,
    Head,
    Tail,
    Jump(usize),
    Search(String),
    FindNext,
    FindPrev,
    SetBookmark,
    JumpBookmark,
    Help,
    Quit,
}

fn parse_step_command(input: &str) -> StepCommand {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return StepCommand::NextLine;
    }
    if let Some(stripped) = trimmed.strip_prefix('/') {
        return StepCommand::Search(stripped.to_string());
    }
    match trimmed {
        "n" => return StepCommand::FindNext,
        "N" => return StepCommand::FindPrev,
        "G" => return StepCommand::Tail,
        "'" => return StepCommand::JumpBookmark,
        _ => {}
    }
    let lower = trimmed.to_ascii_lowercase();
    match lower.as_str() {
        "j" | "next" => StepCommand::NextLine,
        "k" | "p" | "b" | "prev" => StepCommand::PrevLine,
        "g" | "h" | "head" => StepCommand::Head,
        "t" | "tail" => StepCommand::Tail,
        "q" | "quit" => StepCommand::Quit,
        "?" | "help" => StepCommand::Help,
        "m" | "mark" => StepCommand::SetBookmark,
        "jumpmark" | "return" => StepCommand::JumpBookmark,
        _ => {
            if let Some(target) = parse_jump_target(trimmed) {
                StepCommand::Jump(target)
            } else {
                StepCommand::Help
            }
        }
    }
}

fn build_interactive_regex(pattern: &str) -> Result<Regex> {
    if let Some(rest) = pattern.strip_prefix("re:") {
        let trimmed = rest.trim();
        if trimmed.is_empty() {
            bail!("regex pattern cannot be empty");
        }
        return Regex::new(trimmed).map_err(|err| anyhow!("invalid regex: {err}"));
    }

    if pattern.is_empty() {
        bail!("pattern cannot be empty");
    }

    Regex::new(&regex::escape(pattern))
        .map_err(|err| anyhow!("unable to build search regex: {err}"))
}

fn parse_jump_target(raw: &str) -> Option<usize> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let candidate = if trimmed.starts_with('g') || trimmed.starts_with('G') {
        trimmed[1..].trim_start()
    } else {
        trimmed
    };

    let token = candidate.split_whitespace().next().unwrap_or("");
    if token.is_empty() {
        return None;
    }

    token
        .parse::<usize>()
        .ok()
        .and_then(|val| if val == 0 { None } else { Some(val - 1) })
}

fn print_step_help() {
    println!(
        "commands: [Enter]/j next line, b/p/k previous line, g/G head/tail, n/N next/prev match, /pattern set search, m bookmark, ' jump bookmark, number or g <n> jump, q quit"
    );
}

fn to_indices(start_line: usize, end_line: usize, total_lines: usize) -> (usize, usize) {
    let start = start_line
        .saturating_sub(1)
        .min(total_lines.saturating_sub(1));
    let mut end = end_line.saturating_sub(1);
    if end < start {
        end = start;
    }
    (start, (end + 1).min(total_lines))
}

fn parse_range_spec(spec: &str) -> Result<(usize, usize)> {
    let mut parts = spec.split([':', '-']);
    let start = parts
        .next()
        .ok_or_else(|| anyhow!("range spec requires start:end"))?;
    let end = parts
        .next()
        .ok_or_else(|| anyhow!("range spec requires start:end"))?;

    if parts.next().is_some() {
        bail!("range spec should be in the form start:end");
    }

    let start = start.trim().parse::<usize>()?;
    let end = end.trim().parse::<usize>()?;
    if start == 0 || end == 0 {
        bail!("line numbers start at 1");
    }
    if start > end {
        bail!("range start must be <= end");
    }
    Ok((start, end))
}

fn parse_line_context(spec: &str) -> Result<(usize, usize)> {
    let mut parts = spec.split([':', ',']);
    let line = parts
        .next()
        .ok_or_else(|| anyhow!("around spec requires line:context"))?;
    let context = parts
        .next()
        .ok_or_else(|| anyhow!("around spec requires line:context"))?;

    let line = line.trim().parse::<usize>()?;
    let context = context.trim().parse::<usize>()?;
    if line == 0 {
        bail!("line numbers start at 1");
    }

    Ok((line, context))
}

fn build_matcher(pattern: Option<&str>, regex: bool) -> Result<Option<Regex>> {
    let Some(raw) = pattern else {
        return Ok(None);
    };

    let expr = if regex {
        raw.to_string()
    } else {
        regex::escape(raw)
    };

    Regex::new(&expr)
        .map(Some)
        .map_err(|err| anyhow!("invalid search pattern: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_range() {
        assert_eq!(parse_range_spec("10:20").unwrap(), (10, 20));
    }

    #[test]
    fn parse_line_ctx() {
        assert_eq!(parse_line_context("42:5").unwrap(), (42, 5));
    }

    #[test]
    fn highlight_literal() {
        let regex = build_matcher(Some("foo"), false).unwrap().unwrap();
        assert_eq!(
            highlight_line("foo bar foo", Some(&regex)),
            ">>foo<< bar >>foo<<"
        );
    }

    #[test]
    fn parse_step_command_numeric_jump() {
        assert_eq!(parse_step_command("12"), StepCommand::Jump(11));
    }

    #[test]
    fn parse_step_command_goto() {
        assert_eq!(parse_step_command("g 2"), StepCommand::Jump(1));
    }

    #[test]
    fn parse_step_command_invalid_defaults_to_help() {
        assert_eq!(parse_step_command("zzz"), StepCommand::Help);
    }

    #[test]
    fn parse_step_command_search() {
        assert_eq!(
            parse_step_command("/todo"),
            StepCommand::Search("todo".into())
        );
    }

    #[test]
    fn parse_step_command_next_match() {
        assert_eq!(parse_step_command("n"), StepCommand::FindNext);
        assert_eq!(parse_step_command("N"), StepCommand::FindPrev);
    }

    #[test]
    fn parse_step_command_bookmarks() {
        assert_eq!(parse_step_command("m"), StepCommand::SetBookmark);
        assert_eq!(parse_step_command("'"), StepCommand::JumpBookmark);
    }
}
