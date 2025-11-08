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
        return Ok(None);
    }

    Ok(Some(replaced))
}
