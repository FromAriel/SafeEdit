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
    let mut best: Option<(usize, usize, usize, &str)> = None;
    for (line_idx, line) in text.lines().enumerate() {
        let entry = if let Some(pos) = line.find(pattern) {
            (0, line_idx, pos, line)
        } else {
            let score = mismatch_score(line, pattern);
            (score, line_idx, 0, line)
        };

        best = match best {
            Some(current) if entry.0 >= current.0 => Some(current),
            Some(_) | None => Some(entry),
        };

        if entry.0 == 0 {
            break;
        }
    }

    if let Some((_, line_idx, col, line)) = best {
        println!(
            "no exact matches; closest match near line {} column {}:\n  {}",
            line_idx + 1,
            col + 1,
            line.trim()
        );
    } else {
        println!("no similar text found for '{pattern}'");
    }
}

fn mismatch_score(line: &str, pattern: &str) -> usize {
    let snippet = if line.len() >= pattern.len() {
        &line[..pattern.len()]
    } else {
        line
    };
    levenshtein(snippet, pattern)
}

fn levenshtein(a: &str, b: &str) -> usize {
    let mut costs = (0..=b.len()).collect::<Vec<_>>();
    for (i, ca) in a.chars().enumerate() {
        let mut last = i;
        costs[0] = i + 1;
        for (j, cb) in b.chars().enumerate() {
            let new = if ca == cb {
                last
            } else {
                1 + std::cmp::min(std::cmp::min(costs[j], costs[j + 1]), last)
            };
            last = costs[j + 1];
            costs[j + 1] = new;
        }
    }
    costs[b.len()]
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
}
