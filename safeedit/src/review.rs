use anyhow::{Context, Result, anyhow, bail};
use regex::{Captures, Regex};
use std::fs;

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
    pub search: Option<&'a str>,
    pub regex: bool,
}

#[derive(Debug, Clone)]
pub struct ReviewOptions {
    slices: Vec<ReviewSlice>,
    matcher: Option<Regex>,
    follow: bool,
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
        })
    }

    pub fn matcher(&self) -> Option<&Regex> {
        self.matcher.as_ref()
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

    render_content(&decoded, options)?;
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
}
