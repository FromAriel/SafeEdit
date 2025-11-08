use anyhow::{Result, anyhow};
use regex::{Captures, Regex};

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
}

pub fn run_replace(
    entry: &FileEntry,
    encoding: &EncodingStrategy,
    options: &ReplaceOptions,
) -> Result<Option<TransformResult>> {
    let context = TransformContext { entry, encoding };

    run_transform(&context, |decoded| apply_replace(decoded, options))
}

fn apply_replace(decoded: &DecodedText, options: &ReplaceOptions) -> Result<Option<String>> {
    let regex = Regex::new(&options.pattern).map_err(|err| anyhow!("invalid pattern: {err}"))?;
    let template = options.replacement.clone();
    let mut matches = 0usize;

    let make_text = |caps: &Captures<'_>| {
        matches += 1;
        if options.allow_captures {
            let mut output = String::new();
            caps.expand(&template, &mut output);
            output
        } else {
            template.clone()
        }
    };

    let replaced = if let Some(limit) = options.count {
        regex.replacen(&decoded.text, limit, make_text).into_owned()
    } else {
        regex.replace_all(&decoded.text, make_text).into_owned()
    };

    if let Some(expected) = options.expect {
        if matches != expected {
            return Err(anyhow!("expected {expected} matches but found {matches}"));
        }
    }

    if matches == 0 {
        report_suggestions(&decoded.text, &options.pattern);
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
}
