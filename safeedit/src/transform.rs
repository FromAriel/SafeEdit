use std::fs;

use anyhow::{Context, Result};

use crate::encoding::{DecodedText, EncodingStrategy};
use crate::files::FileEntry;

pub struct TransformContext<'a> {
    pub entry: &'a FileEntry,
    pub encoding: &'a EncodingStrategy,
}

pub struct TransformResult {
    pub decoded: DecodedText,
    pub new_text: String,
}

pub fn run_transform<F>(
    ctx: &TransformContext<'_>,
    transformer: F,
) -> Result<Option<TransformResult>>
where
    F: Fn(&DecodedText) -> Result<Option<String>>,
{
    if ctx.entry.metadata.is_probably_binary {
        println!(
            "skipping {} (suspected binary file)",
            ctx.entry.path.display()
        );
        return Ok(None);
    }

    let bytes = fs::read(&ctx.entry.path)
        .with_context(|| format!("failed to read {}", ctx.entry.path.display()))?;
    let decoded = ctx.encoding.decode(&bytes);

    if decoded.had_errors {
        println!(
            "warning: decoding errors encountered for {}; continuing",
            ctx.entry.path.display()
        );
    }

    let Some(new_text) = transformer(&decoded)? else {
        println!("no changes for {}", ctx.entry.path.display());
        return Ok(None);
    };

    Ok(Some(TransformResult { decoded, new_text }))
}
