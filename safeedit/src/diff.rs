use std::path::Path;

use anyhow::Result;
use similar::{ChangeTag, DiffTag, TextDiff};

use crate::logging::{LineSpan, LineSpanKind};

const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const RESET: &str = "\x1b[0m";

pub fn print_diff(old: &str, new: &str, context: usize, colorize: bool) -> Result<()> {
    let diff = TextDiff::configure()
        .algorithm(similar::Algorithm::Myers)
        .diff_lines(old, new);

    for (idx, group) in diff.grouped_ops(context).iter().enumerate() {
        if idx > 0 {
            println!("...");
        }
        for op in group {
            for change in diff.iter_changes(op) {
                let (symbol, style) = match change.tag() {
                    ChangeTag::Delete => ('-', Some(RED)),
                    ChangeTag::Insert => ('+', Some(GREEN)),
                    ChangeTag::Equal => (' ', None),
                };
                if colorize {
                    if let Some(style_code) = style {
                        print!("{style_code}{symbol} {change}{RESET}");
                    } else {
                        print!("{symbol} {change}");
                    }
                } else {
                    print!("{symbol} {change}");
                }
            }
        }
    }

    Ok(())
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
