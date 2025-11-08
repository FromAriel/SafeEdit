use std::io::{self, Write};
use std::path::Path;

use anyhow::Result;
use similar::{ChangeTag, DiffTag, TextDiff};

use crate::PagerMode;
use crate::logging::{LineSpan, LineSpanKind};

const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const RESET: &str = "\x1b[0m";

pub const DIFF_MAX_LINES: usize = 5_000;
pub const DIFF_MAX_BYTES: usize = 5 * 1024 * 1024;
pub const DIFF_MAX_LINE_BYTES: usize = 64 * 1024;
pub const DIFF_LINE_TRUNCATION_SUFFIX: &str = "... (line truncated)\n";
const PAGE_LINES: usize = 200;

pub struct DiffDisplayConfig {
    pub context: usize,
    pub colorize: bool,
    pub pager_mode: PagerMode,
    pub interactive: bool,
}

pub fn display_diff(old: &str, new: &str, config: &DiffDisplayConfig) -> Result<()> {
    let buffer = DiffBuffer::build(old, new, config.context, config.colorize);
    if buffer.lines.is_empty() {
        return Ok(());
    }

    let viewer_requested = match config.pager_mode {
        PagerMode::Always => true,
        PagerMode::Never => false,
        PagerMode::Auto => buffer.line_count() > PAGE_LINES,
    };

    if viewer_requested && config.interactive {
        run_internal_pager(&buffer)?;
    } else {
        if viewer_requested && !config.interactive && config.pager_mode != PagerMode::Never {
            println!("(pager disabled: non-interactive session; showing diff inline)");
        }
        buffer.print_inline();
    }

    buffer.print_warnings();
    Ok(())
}

struct DiffBuffer {
    lines: Vec<String>,
    total_bytes: usize,
    truncated: bool,
    truncate_reason: Option<TruncateReason>,
    line_truncations: usize,
}

#[derive(Clone, Copy)]
enum TruncateReason {
    LineCount,
    ByteCount,
}

impl DiffBuffer {
    fn build(old: &str, new: &str, context: usize, colorize: bool) -> Self {
        let diff = TextDiff::configure()
            .algorithm(similar::Algorithm::Myers)
            .diff_lines(old, new);

        let mut buffer = DiffBuffer {
            lines: Vec::new(),
            total_bytes: 0,
            truncated: false,
            truncate_reason: None,
            line_truncations: 0,
        };

        'outer: for (idx, group) in diff.grouped_ops(context).iter().enumerate() {
            if idx > 0 && !buffer.push_line("...\n".to_string()) {
                break;
            }

            for op in group {
                for change in diff.iter_changes(op) {
                    let (symbol, style) = match change.tag() {
                        ChangeTag::Delete => ('-', Some(RED)),
                        ChangeTag::Insert => ('+', Some(GREEN)),
                        ChangeTag::Equal => (' ', None),
                    };
                    let line = if colorize {
                        if let Some(style_code) = style {
                            format!("{style_code}{symbol} {change}{RESET}")
                        } else {
                            format!("{symbol} {change}")
                        }
                    } else {
                        format!("{symbol} {change}")
                    };

                    if !buffer.push_line(line) {
                        break 'outer;
                    }
                }
            }
        }

        buffer
    }

    fn push_line(&mut self, mut line: String) -> bool {
        let exceeded = line.len() > DIFF_MAX_LINE_BYTES;
        if exceeded {
            truncate_line(&mut line);
            self.line_truncations += 1;
        }
        self.total_bytes = self.total_bytes.saturating_add(line.len());
        self.lines.push(line);

        if self.lines.len() >= DIFF_MAX_LINES {
            self.truncated = true;
            self.truncate_reason = Some(TruncateReason::LineCount);
            return false;
        }

        if self.total_bytes >= DIFF_MAX_BYTES {
            self.truncated = true;
            self.truncate_reason = Some(TruncateReason::ByteCount);
            return false;
        }

        true
    }

    fn line_count(&self) -> usize {
        self.lines.len()
    }

    fn print_inline(&self) {
        for line in &self.lines {
            print!("{line}");
        }
    }

    fn print_warnings(&self) {
        if let Some(reason) = self.truncate_reason {
            match reason {
                TruncateReason::LineCount => println!(
                    "(diff truncated at ~{DIFF_MAX_LINES} lines; rerun with --pager never and narrower targets to view everything)"
                ),
                TruncateReason::ByteCount => println!(
                    "(diff truncated after ~{DIFF_MAX_BYTES} bytes; rerun with --pager never and narrower targets to view everything)"
                ),
            }
        }

        if self.line_truncations > 0 {
            println!(
                "(note: {} diff line(s) exceeded {} bytes and were truncated; rerun with --pager never to inspect the full line)",
                self.line_truncations, DIFF_MAX_LINE_BYTES
            );
        }
    }
}

fn truncate_line(line: &mut String) {
    if line.len() <= DIFF_LINE_TRUNCATION_SUFFIX.len() {
        line.clear();
        line.push_str(DIFF_LINE_TRUNCATION_SUFFIX);
        return;
    }

    let target_len = DIFF_MAX_LINE_BYTES.saturating_sub(DIFF_LINE_TRUNCATION_SUFFIX.len());
    if line.len() > target_len {
        line.truncate(target_len);
    }
    if line.ends_with('\n') {
        line.pop();
    }
    line.push_str(DIFF_LINE_TRUNCATION_SUFFIX);
}

fn run_internal_pager(buffer: &DiffBuffer) -> Result<()> {
    if buffer.lines.is_empty() {
        println!("(diff is empty)");
        return Ok(());
    }

    let total_pages = buffer.line_count().div_ceil(PAGE_LINES);
    let mut page = 0usize;

    loop {
        render_page(buffer, page, total_pages);
        if total_pages == 1 {
            break;
        }

        print!("pager [Enter/n=next, p=prev, g <line>=goto, h=head, t=tail, q=quit]: ");
        io::stdout().flush()?;
        let mut input = String::new();
        let bytes = io::stdin().read_line(&mut input)?;
        if bytes == 0 {
            println!("stdin closed; leaving diff pager.");
            break;
        }

        match parse_pager_command(input.trim()) {
            PagerCommand::Next => {
                if page + 1 >= total_pages {
                    break;
                }
                page += 1;
            }
            PagerCommand::Prev => {
                if page == 0 {
                    println!("(already at beginning)");
                } else {
                    page -= 1;
                }
            }
            PagerCommand::Head => page = 0,
            PagerCommand::Tail => page = total_pages.saturating_sub(1),
            PagerCommand::GotoLine(line) => {
                if line == 0 {
                    println!("line numbers start at 1");
                    continue;
                }
                let target = line
                    .saturating_sub(1)
                    .min(buffer.line_count().saturating_sub(1));
                page = target / PAGE_LINES;
            }
            PagerCommand::Quit => break,
            PagerCommand::Help => print_pager_help(),
        }
    }

    Ok(())
}

fn render_page(buffer: &DiffBuffer, page: usize, total_pages: usize) {
    let start = page * PAGE_LINES;
    let mut end = start + PAGE_LINES;
    if end > buffer.line_count() {
        end = buffer.line_count();
    }
    println!(
        "--- diff page {}/{} (lines {}-{}) ---",
        page + 1,
        total_pages,
        start + 1,
        end
    );
    for line in &buffer.lines[start..end] {
        print!("{line}");
    }
    if end >= buffer.line_count() {
        println!("(end of diff)");
    }
}

#[derive(Debug)]
enum PagerCommand {
    Next,
    Prev,
    Head,
    Tail,
    GotoLine(usize),
    Quit,
    Help,
}

fn parse_pager_command(input: &str) -> PagerCommand {
    if input.is_empty() || input.eq_ignore_ascii_case("n") {
        return PagerCommand::Next;
    }
    if input.eq_ignore_ascii_case("p") {
        return PagerCommand::Prev;
    }
    if input.eq_ignore_ascii_case("h") {
        return PagerCommand::Head;
    }
    if input.eq_ignore_ascii_case("t") {
        return PagerCommand::Tail;
    }
    if input.eq_ignore_ascii_case("q") {
        return PagerCommand::Quit;
    }
    if input.eq_ignore_ascii_case("?") {
        return PagerCommand::Help;
    }
    let mut parts = input.split_whitespace();
    if let Some(cmd) = parts.next() {
        if cmd.eq_ignore_ascii_case("g") {
            if let Some(num) = parts.next() {
                if let Ok(value) = num.parse::<usize>() {
                    return PagerCommand::GotoLine(value);
                }
            }
            return PagerCommand::Help;
        }
        if cmd.chars().all(|ch| ch.is_ascii_digit()) {
            if let Ok(value) = cmd.parse::<usize>() {
                return PagerCommand::GotoLine(value);
            }
        }
    }
    PagerCommand::Help
}

fn print_pager_help() {
    println!("Commands: Enter/n=next, p=previous, g <line>=jump to line, h=head, t=tail, q=quit.");
}

pub fn summarize_lines(old: &str, new: &str) -> String {
    let diff = TextDiff::from_lines(old, new);
    let mut parts = Vec::new();
    for op in diff.ops() {
        match op.tag() {
            DiffTag::Equal => {}
            DiffTag::Delete | DiffTag::Replace => {
                let start = op.old_range().start + 1;
                let end = op.old_range().end;
                if start == end {
                    parts.push(format!("L{start}"));
                } else {
                    parts.push(format!("L{start}-L{end}"));
                }
            }
            DiffTag::Insert => {
                let start = op.new_range().start + 1;
                parts.push(format!("+L{start}"));
            }
        }
    }

    if parts.is_empty() {
        "no-change".into()
    } else {
        parts.join(", ")
    }
}

pub fn collect_line_spans(old: &str, new: &str) -> Vec<LineSpan> {
    let diff = TextDiff::from_lines(old, new);
    let mut spans = Vec::new();
    for op in diff.ops() {
        match op.tag() {
            DiffTag::Equal => {}
            DiffTag::Delete | DiffTag::Replace => {
                let start = op.old_range().start + 1;
                let end = op.old_range().end;
                if start <= end {
                    spans.push(LineSpan {
                        kind: LineSpanKind::Modified,
                        start,
                        end,
                    });
                }
            }
            DiffTag::Insert => {
                let start = op.new_range().start + 1;
                let end = op.new_range().end;
                if start <= end {
                    spans.push(LineSpan {
                        kind: LineSpanKind::Added,
                        start,
                        end,
                    });
                }
            }
        }
    }
    spans
}

pub fn unified_diff(
    old_path: &Path,
    new_path: &Path,
    old: &str,
    new: &str,
    context: usize,
) -> String {
    let old_label = old_path.display().to_string();
    let new_label = new_path.display().to_string();
    TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(context)
        .header(&old_label, &new_label)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_line_spans_marks_modified_and_added() {
        let old = "one\nold\nthree\n";
        let new = "one\nnew\nthree\nextra\n";
        let spans = collect_line_spans(old, new);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].kind, LineSpanKind::Modified);
        assert_eq!(spans[0].start, 2);
        assert_eq!(spans[0].end, 2);
        assert_eq!(spans[1].kind, LineSpanKind::Added);
        assert_eq!(spans[1].start, 4);
        assert_eq!(spans[1].end, 4);
    }
}
