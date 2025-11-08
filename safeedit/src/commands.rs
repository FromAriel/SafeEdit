use anyhow::{Result, anyhow, bail};
use regex::{Regex, RegexBuilder};

use crate::BlockMode;
use crate::encoding::{DecodedText, EncodingStrategy};
use crate::files::FileEntry;
use crate::transform::{TransformContext, TransformResult, run_transform};

#[derive(Debug, Clone)]
pub struct ReplaceOptions {
    pub pattern: String,
    pub replacement: String,
    pub allow_captures: bool,
    pub count: Option<usize>,
    pub expect: Option<usize>,
    pub after_line: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct BlockOptions {
    pub start_marker: String,
    pub end_marker: String,
    pub mode: BlockMode,
    pub body: String,
}

#[derive(Debug, Clone)]
pub struct RenameOptions {
    pub from: String,
    pub to: String,
    pub word_boundary: bool,
    pub case_aware: bool,
}

pub fn run_replace(
    entry: &FileEntry,
    encoding: &EncodingStrategy,
    options: &ReplaceOptions,
) -> Result<Option<TransformResult>> {
    let context = TransformContext { entry, encoding };

    run_transform(&context, |decoded| apply_replace(decoded, options))
}

pub fn run_block(
    entry: &FileEntry,
    encoding: &EncodingStrategy,
    options: &BlockOptions,
) -> Result<Option<TransformResult>> {
    let context = TransformContext { entry, encoding };
    run_transform(&context, |decoded| apply_block(decoded, options))
}

pub fn run_rename(
    entry: &FileEntry,
    encoding: &EncodingStrategy,
    options: &RenameOptions,
) -> Result<Option<TransformResult>> {
    let context = TransformContext { entry, encoding };
    run_transform(&context, |decoded| apply_rename(decoded, options))
}

fn apply_replace(decoded: &DecodedText, options: &ReplaceOptions) -> Result<Option<String>> {
    let regex = Regex::new(&options.pattern).map_err(|err| anyhow!("invalid pattern: {err}"))?;
    let mut output = String::with_capacity(decoded.text.len());
    let mut last_end = 0usize;
    let mut replacements = 0usize;
    let mut filtered_by_line = 0usize;
    let mut capture_buffer = String::new();
    let line_index = options.after_line.map(|_| LineIndex::new(&decoded.text));
    let template = options.replacement.as_str();

    for caps in regex.captures_iter(&decoded.text) {
        let matched = caps.get(0).expect("match group");
        let eligible = if let (Some(limit), Some(index)) = (options.after_line, line_index.as_ref())
        {
            index.line_at(matched.start()) > limit
        } else {
            true
        };

        if !eligible {
            filtered_by_line += 1;
            continue;
        }

        if let Some(limit) = options.count {
            if replacements >= limit {
                break;
            }
        }

        output.push_str(&decoded.text[last_end..matched.start()]);

        if options.allow_captures {
            capture_buffer.clear();
            caps.expand(template, &mut capture_buffer);
            output.push_str(&capture_buffer);
        } else {
            output.push_str(template);
        }

        last_end = matched.end();
        replacements += 1;
    }

    if replacements == 0 {
        if options.after_line.is_some() && filtered_by_line > 0 {
            println!(
                "no matches after line {}; {} occurrence(s) were at or before that line",
                options.after_line.unwrap(),
                filtered_by_line
            );
            return Ok(None);
        }
        report_suggestions(&decoded.text, &options.pattern);
        return Ok(None);
    }

    output.push_str(&decoded.text[last_end..]);

    if let Some(expected) = options.expect {
        if replacements != expected {
            return Err(anyhow!(
                "expected {expected} matches but found {replacements}"
            ));
        }
    }

    Ok(Some(output))
}

fn apply_block(decoded: &DecodedText, options: &BlockOptions) -> Result<Option<String>> {
    let text = &decoded.text;
    let Some(start_pos) = text.find(&options.start_marker) else {
        bail!("start marker '{}' not found", options.start_marker);
    };
    let after_start = start_pos + options.start_marker.len();
    let Some(rel_end) = text[after_start..].find(&options.end_marker) else {
        bail!(
            "end marker '{}' not found after start marker",
            options.end_marker
        );
    };
    let end_pos = after_start + rel_end;
    let existing = &text[after_start..end_pos];
    if matches!(options.mode, BlockMode::Insert) && !existing.trim().is_empty() {
        bail!("insert mode requires the block region to be empty");
    }

    let indent = block_indent(text, start_pos);
    let desired = adjust_block_body(existing, &options.body, text, &indent);

    if existing == desired {
        return Ok(None);
    }

    let mut new_text =
        String::with_capacity(text.len().saturating_sub(existing.len()) + desired.len());
    new_text.push_str(&text[..after_start]);
    new_text.push_str(&desired);
    new_text.push_str(&text[end_pos..]);

    if new_text == decoded.text {
        return Ok(None);
    }

    Ok(Some(new_text))
}

fn adjust_block_body(existing: &str, requested: &str, full_text: &str, indent: &str) -> String {
    let newline = preferred_line_ending(existing, full_text);
    let mut body = normalize_line_endings_to(requested, "\n");

    if has_leading_linebreak(existing) && !body.starts_with('\n') {
        body = format!("\n{body}");
    }
    if has_trailing_linebreak(existing) && !body.ends_with('\n') {
        body.push('\n');
    }

    let mut rebuilt = String::with_capacity(body.len() + indent.len() * 4);
    let trailing_indent = extract_trailing_indent(existing);

    let mut chunks: Vec<&str> = body.split_inclusive('\n').collect();
    if chunks.is_empty() {
        chunks.push(body.as_str());
    }
    for segment in chunks {
        let (line, has_newline) = if let Some(stripped) = segment.strip_suffix('\n') {
            (stripped, true)
        } else {
            (segment, false)
        };

        if needs_indent(line, indent) {
            rebuilt.push_str(indent);
        }
        rebuilt.push_str(line);
        if has_newline {
            rebuilt.push('\n');
        }
    }

    if !trailing_indent.is_empty() {
        let needle = format!("\n{trailing_indent}");
        if !rebuilt.ends_with(&needle) {
            if !rebuilt.ends_with('\n') {
                rebuilt.push('\n');
            }
            rebuilt.push_str(trailing_indent);
        }
    }

    restore_line_endings(&rebuilt, newline)
}

fn preferred_line_ending(block: &str, doc: &str) -> &'static str {
    if block.contains("\r\n") || doc.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

fn has_leading_linebreak(text: &str) -> bool {
    text.starts_with("\r\n") || text.starts_with('\n')
}

fn has_trailing_linebreak(text: &str) -> bool {
    text.ends_with("\r\n") || (text.ends_with('\n') && !text.ends_with("\r\n"))
}

fn normalize_line_endings_to(input: &str, newline: &str) -> String {
    if input.is_empty() {
        return String::new();
    }
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\r' => {
                if matches!(chars.peek(), Some('\n')) {
                    chars.next();
                }
                result.push_str(newline);
            }
            '\n' => result.push_str(newline),
            _ => result.push(ch),
        }
    }
    result
}

fn restore_line_endings(text: &str, newline: &str) -> String {
    if newline == "\n" {
        text.to_string()
    } else {
        text.replace('\n', newline)
    }
}

fn needs_indent(line: &str, indent: &str) -> bool {
    if indent.is_empty() {
        return false;
    }
    match line.chars().next() {
        Some(' ') | Some('\t') => false,
        Some(_) => true,
        None => false,
    }
}

fn block_indent(text: &str, marker_start: usize) -> String {
    let line_start = text[..marker_start]
        .rfind('\n')
        .map(|idx| idx + 1)
        .unwrap_or(0);
    text[line_start..marker_start]
        .chars()
        .take_while(|c| matches!(c, ' ' | '\t'))
        .collect()
}

fn extract_trailing_indent(existing: &str) -> &str {
    if let Some(pos) = existing.rfind('\n') {
        let tail = &existing[pos + 1..];
        if !tail.is_empty() && tail.chars().all(|c| matches!(c, ' ' | '\t')) {
            return tail;
        }
    }
    ""
}

fn apply_rename(decoded: &DecodedText, options: &RenameOptions) -> Result<Option<String>> {
    let mut pattern = regex::escape(&options.from);
    if options.word_boundary {
        pattern = format!(r"\b{pattern}\b");
    }
    let mut builder = RegexBuilder::new(&pattern);
    if options.case_aware {
        builder.case_insensitive(true);
    }
    let regex = builder
        .build()
        .map_err(|err| anyhow!("invalid pattern: {err}"))?;
    let replacement = options.to.clone();
    let mut matches = 0usize;

    let replaced = regex
        .replace_all(&decoded.text, |caps: &regex::Captures<'_>| {
            matches += 1;
            if options.case_aware {
                adjust_case(&caps[0], &replacement)
            } else {
                replacement.clone()
            }
        })
        .into_owned();

    if matches == 0 {
        println!(
            "rename: no matches for '{}'{}",
            options.from,
            if options.word_boundary {
                " with word-boundary guard"
            } else {
                ""
            }
        );
        report_suggestions(&decoded.text, &options.from);
        return Ok(None);
    }

    Ok(Some(replaced))
}

fn report_suggestions(text: &str, pattern: &str) {
    const LIMIT: usize = 3;
    let suggestions = collect_suggestions(text, pattern, LIMIT);
    if suggestions.is_empty() {
        println!("no similar text found for '{pattern}'");
        return;
    }

    println!("no exact matches; closest candidates:");
    for suggestion in suggestions {
        println!(
            "  - line {} column {} (score {}): {}",
            suggestion.line_idx + 1,
            suggestion.column + 1,
            suggestion.score,
            suggestion.line.trim()
        );
        println!("    snippet: {}", suggestion.snippet);
        let (pattern_view, diff_line) = render_diff_hint(&suggestion.snippet, pattern);
        println!("    pattern: {pattern_view}");
        println!("             {diff_line}");
    }
}

#[derive(Debug, Clone)]
struct Suggestion {
    score: usize,
    line_idx: usize,
    column: usize,
    line: String,
    snippet: String,
}

fn collect_suggestions(text: &str, pattern: &str, limit: usize) -> Vec<Suggestion> {
    if pattern.is_empty() {
        return vec![];
    }

    let mut suggestions = Vec::new();
    for (line_idx, line) in text.lines().enumerate() {
        if let Some((score, column, snippet)) = best_window(line, pattern) {
            suggestions.push(Suggestion {
                score,
                line_idx,
                column,
                line: line.to_string(),
                snippet,
            });
        }
    }

    suggestions.sort_by(|a, b| {
        a.score
            .cmp(&b.score)
            .then(a.line_idx.cmp(&b.line_idx))
            .then(a.column.cmp(&b.column))
    });
    if suggestions.len() > limit {
        suggestions.truncate(limit);
    }
    suggestions
}

fn best_window(line: &str, pattern: &str) -> Option<(usize, usize, String)> {
    if line.is_empty() {
        return None;
    }

    let line_chars: Vec<char> = line.chars().collect();
    let pat_len = std::cmp::max(pattern.chars().count(), 1);
    let mut best: Option<(usize, usize, String)> = None;

    for start in 0..line_chars.len() {
        if start >= line_chars.len() {
            break;
        }

        let lengths = [pat_len, pat_len.saturating_add(2)];
        for &len in &lengths {
            let end = (start + len).min(line_chars.len());
            if end <= start {
                continue;
            }
            let snippet: String = line_chars[start..end].iter().collect();
            let score = levenshtein(&snippet, pattern);
            let candidate = (score, start, snippet);

            best = match best.take() {
                None => Some(candidate),
                Some(current)
                    if score < current.0
                        || (score == current.0
                            && (start < current.1
                                || (start == current.1
                                    && candidate.2.len() < current.2.len()))) =>
                {
                    Some(candidate)
                }
                Some(current) => Some(current),
            };
        }
    }

    best
}

fn render_diff_hint(snippet: &str, pattern: &str) -> (String, String) {
    let snippet_chars: Vec<char> = snippet.chars().collect();
    let pattern_chars: Vec<char> = pattern.chars().collect();
    let width = snippet_chars.len().max(pattern_chars.len());
    let mut diff_line = String::with_capacity(width);
    for idx in 0..width {
        let sc = snippet_chars.get(idx).copied().unwrap_or(' ');
        let pc = pattern_chars.get(idx).copied().unwrap_or(' ');
        diff_line.push(if sc == pc { ' ' } else { '^' });
    }
    (pattern.to_string(), diff_line)
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    if a_chars.is_empty() {
        return b_chars.len();
    }
    if b_chars.is_empty() {
        return a_chars.len();
    }

    let mut previous: Vec<usize> = (0..=b_chars.len()).collect();
    let mut current = vec![0; b_chars.len() + 1];

    for (i, &ca) in a_chars.iter().enumerate() {
        current[0] = i + 1;
        for (j, &cb) in b_chars.iter().enumerate() {
            let substitution_cost = if ca == cb { 0 } else { 1 };
            current[j + 1] = std::cmp::min(
                std::cmp::min(current[j] + 1, previous[j + 1] + 1),
                previous[j] + substitution_cost,
            );
        }
        std::mem::swap(&mut previous, &mut current);
    }

    previous[b_chars.len()]
}

struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    fn new(text: &str) -> Self {
        let mut starts = Vec::new();
        starts.push(0);
        for (idx, ch) in text.char_indices() {
            if ch == '\n' {
                let next = idx + ch.len_utf8();
                if next <= text.len() {
                    starts.push(next);
                }
            }
        }
        Self { starts }
    }

    fn line_at(&self, offset: usize) -> usize {
        match self.starts.binary_search(&offset) {
            Ok(pos) => pos + 1,
            Err(pos) => pos,
        }
    }
}

fn adjust_case(source: &str, target: &str) -> String {
    match detect_case_kind(source) {
        CaseKind::Upper => target.to_uppercase(),
        CaseKind::Lower => target.to_lowercase(),
        CaseKind::Capitalized => capitalize(target),
        CaseKind::Mixed => target.to_string(),
    }
}

fn capitalize(value: &str) -> String {
    let mut chars = value.chars();
    let mut output = String::with_capacity(value.len());
    if let Some(first) = chars.next() {
        output.extend(first.to_uppercase());
    }
    for ch in chars {
        output.extend(ch.to_lowercase());
    }
    output
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaseKind {
    Upper,
    Lower,
    Capitalized,
    Mixed,
}

fn detect_case_kind(text: &str) -> CaseKind {
    let mut has_alpha = false;
    let mut all_upper = true;
    let mut all_lower = true;
    for (idx, ch) in text.chars().enumerate() {
        if ch.is_alphabetic() {
            has_alpha = true;
            if ch.is_uppercase() {
                all_lower = false;
            } else if ch.is_lowercase() {
                all_upper = false;
            } else {
                all_upper = false;
                all_lower = false;
            }
            if idx > 0 && ch.is_uppercase() {
                all_lower = false;
            }
        }
    }

    if !has_alpha {
        return CaseKind::Mixed;
    }
    if all_upper {
        CaseKind::Upper
    } else if all_lower {
        CaseKind::Lower
    } else if text
        .chars()
        .next()
        .map(|c| c.is_uppercase())
        .unwrap_or(false)
        && text.chars().skip(1).all(|c| !c.is_uppercase())
    {
        CaseKind::Capitalized
    } else {
        CaseKind::Mixed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levenshtein_exact() {
        assert_eq!(levenshtein("foo", "foo"), 0);
    }

    #[test]
    fn levenshtein_single_change() {
        assert_eq!(levenshtein("foo", "foa"), 1);
    }

    #[test]
    fn levenshtein_handles_unicode() {
        assert_eq!(levenshtein("café", "cafe"), 1);
        assert_eq!(levenshtein("naïve", "naive"), 1);
    }

    #[test]
    fn collect_suggestions_orders_by_score() {
        let text = "alpha beta\naplha bent\nsomething else";
        let suggestions = collect_suggestions(text, "alpha beta", 2);
        assert_eq!(suggestions.len(), 2);
        assert_eq!(suggestions[0].line_idx, 0);
        assert_eq!(suggestions[0].score, 0);
        assert_eq!(suggestions[1].line_idx, 1);
        assert!(suggestions[1].score > 0);
    }

    #[test]
    fn best_window_handles_multibyte_chars() {
        let line = "café example";
        let pattern = "cafe";
        let result = best_window(line, pattern).expect("suggestion");
        assert_eq!(result.1, 0);
        assert!(result.0 <= 1);
    }

    #[test]
    fn render_diff_marks_variances() {
        let (pattern_line, diff) = render_diff_hint("alpha", "alpah");
        assert_eq!(pattern_line, "alpah");
        assert_eq!(diff.trim_end(), "   ^^");
    }

    #[test]
    fn replace_after_line_skips_early_matches() {
        let decoded = decoded_text("alpha\nfoo\nbeta\nfoo\n");
        let options = literal_options("foo", "FOO", Some(2));
        let replaced = apply_replace(&decoded, &options)
            .expect("replace")
            .expect("text");
        assert_eq!(replaced, "alpha\nfoo\nbeta\nFOO\n");
    }

    #[test]
    fn replace_after_line_returns_none_when_no_late_matches() {
        let decoded = decoded_text("foo\nfoo\n");
        let options = literal_options("foo", "FOO", Some(5));
        let result = apply_replace(&decoded, &options).expect("replace");
        assert!(result.is_none());
    }

    #[test]
    fn rename_word_boundary_and_case_aware() {
        let decoded = decoded_text("Foo foo FOO");
        let options = RenameOptions {
            from: "foo".into(),
            to: "bar".into(),
            word_boundary: true,
            case_aware: true,
        };
        let replaced = apply_rename(&decoded, &options)
            .expect("rename")
            .expect("text");
        assert_eq!(replaced, "Bar bar BAR");
    }

    #[test]
    fn rename_reports_when_no_match() {
        let decoded = decoded_text("alpha beta");
        let options = RenameOptions {
            from: "nope".into(),
            to: "noop".into(),
            word_boundary: true,
            case_aware: false,
        };
        let result = apply_rename(&decoded, &options).expect("rename");
        assert!(result.is_none());
    }

    #[test]
    fn block_replace_swaps_body() {
        let decoded = decoded_text("/*start*/\nold\n/*end*/");
        let options = BlockOptions {
            start_marker: "/*start*/".into(),
            end_marker: "/*end*/".into(),
            mode: BlockMode::Replace,
            body: "\nnew\n".into(),
        };
        let replaced = apply_block(&decoded, &options)
            .expect("block")
            .expect("text");
        assert_eq!(replaced, "/*start*/\nnew\n/*end*/");
    }

    #[test]
    fn block_replace_injects_missing_linebreaks() {
        let decoded = decoded_text("// begin\nold\n// end\n");
        let options = BlockOptions {
            start_marker: "// begin".into(),
            end_marker: "// end".into(),
            mode: BlockMode::Replace,
            body: "updated line".into(),
        };
        let replaced = apply_block(&decoded, &options)
            .expect("block")
            .expect("text");
        assert_eq!(replaced, "// begin\nupdated line\n// end\n");
    }

    #[test]
    fn block_replace_keeps_inline_segments_flat() {
        let decoded = decoded_text("/*start*/old/*end*/");
        let options = BlockOptions {
            start_marker: "/*start*/".into(),
            end_marker: "/*end*/".into(),
            mode: BlockMode::Replace,
            body: "new".into(),
        };
        let replaced = apply_block(&decoded, &options)
            .expect("block")
            .expect("text");
        assert_eq!(replaced, "/*start*/new/*end*/");
    }

    #[test]
    fn block_insert_errors_when_region_not_empty() {
        let decoded = decoded_text("// begin\nkeep\n// end");
        let options = BlockOptions {
            start_marker: "// begin".into(),
            end_marker: "// end".into(),
            mode: BlockMode::Insert,
            body: "\nnew\n".into(),
        };
        let err = apply_block(&decoded, &options).expect_err("should fail");
        assert!(format!("{err:#}").contains("insert mode"));
    }

    fn decoded_text(text: &str) -> DecodedText {
        EncodingStrategy::new(None)
            .expect("strategy")
            .decode(text.as_bytes())
    }

    fn literal_options(
        pattern: &str,
        replacement: &str,
        after_line: Option<usize>,
    ) -> ReplaceOptions {
        ReplaceOptions {
            pattern: regex::escape(pattern),
            replacement: replacement.to_string(),
            allow_captures: false,
            count: None,
            expect: None,
            after_line,
        }
    }
}
