use std::fs;
use std::mem;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use diffy::Patch;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchKind {
    Modify,
    Create,
    Delete,
    Rename,
}

#[derive(Debug)]
pub struct FilePatch {
    pub source: PathBuf,
    pub index: usize,
    pub patch_text: String,
    pub kind: PatchKind,
    pub old_path: Option<PathBuf>,
    pub new_path: Option<PathBuf>,
}

pub fn load_file_patches(path: &Path) -> Result<Vec<FilePatch>> {
    let raw =
        fs::read_to_string(path).with_context(|| format!("reading patch {}", path.display()))?;
    let segments = split_segments(&raw)?;
    let mut patches = Vec::new();
    for (idx, segment) in segments.into_iter().enumerate() {
        Patch::from_str(&segment.body).map_err(|err| {
            anyhow!(
                "failed to parse patch {} segment {}: {err}",
                path.display(),
                idx + 1
            )
        })?;
        let (kind, old_path, new_path) = classify_paths(&segment.old_label, &segment.new_label)
            .with_context(|| {
                format!(
                    "segment {} in {} has unsupported paths",
                    idx + 1,
                    path.display()
                )
            })?;
        patches.push(FilePatch {
            source: path.to_path_buf(),
            index: idx + 1,
            patch_text: segment.body,
            kind,
            old_path,
            new_path,
        });
    }
    Ok(patches)
}

struct Segment {
    old_label: String,
    new_label: String,
    body: String,
}

fn split_segments(text: &str) -> Result<Vec<Segment>> {
    let mut segments = Vec::new();
    let mut buffer = String::new();
    let mut old_label: Option<String> = None;
    let mut new_label: Option<String> = None;
    let mut in_segment = false;

    for chunk in text.split_inclusive('\n') {
        let cleaned = chunk.replace('\r', "");
        let trimmed = cleaned.trim_end_matches('\n');

        if trimmed.starts_with("diff --") {
            if in_segment {
                finalize_segment(&mut segments, &mut buffer, &mut old_label, &mut new_label)?;
                buffer.clear();
                old_label = None;
                new_label = None;
                in_segment = false;
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("--- ") {
            if in_segment {
                finalize_segment(&mut segments, &mut buffer, &mut old_label, &mut new_label)?;
            }
            buffer.clear();
            buffer.push_str(&cleaned);
            old_label = Some(rest.trim().to_string());
            new_label = None;
            in_segment = true;
            continue;
        }

        if !in_segment {
            continue;
        }

        if new_label.is_none() {
            if let Some(rest) = trimmed.strip_prefix("+++ ") {
                new_label = Some(rest.trim().to_string());
                buffer.push_str(&cleaned);
                continue;
            }
        }

        buffer.push_str(&cleaned);
    }

    if in_segment {
        finalize_segment(&mut segments, &mut buffer, &mut old_label, &mut new_label)?;
    }

    Ok(segments)
}

fn finalize_segment(
    segments: &mut Vec<Segment>,
    buffer: &mut String,
    old_label: &mut Option<String>,
    new_label: &mut Option<String>,
) -> Result<()> {
    let Some(old) = old_label.take() else {
        bail!("patch segment missing --- header");
    };
    let Some(new) = new_label.take() else {
        bail!("patch segment missing +++ header");
    };
    let body = mem::take(buffer);
    segments.push(Segment {
        old_label: old,
        new_label: new,
        body,
    });
    Ok(())
}

fn classify_paths(
    old_label: &str,
    new_label: &str,
) -> Result<(PatchKind, Option<PathBuf>, Option<PathBuf>)> {
    let old_path = label_to_path(old_label);
    let new_path = label_to_path(new_label);
    match (old_path, new_path) {
        (Some(old), Some(new)) => {
            if old == new {
                Ok((PatchKind::Modify, Some(old), Some(new)))
            } else {
                Ok((PatchKind::Rename, Some(old), Some(new)))
            }
        }
        (None, Some(new)) => Ok((PatchKind::Create, None, Some(new))),
        (Some(old), None) => Ok((PatchKind::Delete, Some(old), None)),
        (None, None) => bail!("patch is missing both old and new file labels"),
    }
}

fn label_to_path(label: &str) -> Option<PathBuf> {
    let trimmed = label.trim();
    if trimmed == "/dev/null" {
        return None;
    }
    let unquoted = trimmed.trim_matches('"');
    let stripped = if let Some(rest) = unquoted.strip_prefix("a/") {
        rest
    } else if let Some(rest) = unquoted.strip_prefix("b/") {
        rest
    } else {
        unquoted
    };
    let cleaned = stripped.trim_start_matches("./");
    if cleaned.is_empty() {
        None
    } else {
        Some(PathBuf::from(cleaned))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_segments_finds_multiple_entries() {
        let text = "\
diff --git a/foo.txt b/foo.txt
index 111..222 100644
--- a/foo.txt
+++ b/foo.txt
@@ -1 +1 @@
-old
+new

--- a/bar.txt
+++ b/bar.txt
@@ -2 +2 @@
-before
+after
";
        let segments = split_segments(text).expect("segments");
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].old_label, "a/foo.txt");
        assert_eq!(segments[1].new_label, "b/bar.txt");
    }

    #[test]
    fn split_segments_handles_git_headers_between_files() {
        let text = "\
diff --git a/foo.txt b/foo.txt
index 111..222 100644
--- a/foo.txt
+++ b/foo.txt
@@ -1,2 +1,3 @@
 line1
-line2
+line2 edit
+line3

diff --git a/new.txt b/new.txt
new file mode 100644
index 0000000..3333333
--- /dev/null
+++ b/new.txt
@@ -0,0 +1,2 @@
+hello
+world
";
        let segments = split_segments(text).expect("segments");
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].old_label, "a/foo.txt");
        assert_eq!(segments[1].new_label, "b/new.txt");
        assert!(
            !segments[0].body.contains("diff --git"),
            "first body should not include next diff header:\n{}",
            segments[0].body
        );
        assert!(
            !segments[1].body.contains("diff --git"),
            "second body should not include diff header:\n{}",
            segments[1].body
        );
    }

    #[test]
    fn label_to_path_strips_prefixes() {
        let path = label_to_path("a/src/main.rs").expect("path");
        assert_eq!(path, PathBuf::from("src/main.rs"));
    }

    #[test]
    fn classify_detects_create_and_delete() {
        let (kind_new, old_new, new_new) = classify_paths("/dev/null", "b/new.rs").expect("create");
        assert_eq!(kind_new, PatchKind::Create);
        assert!(old_new.is_none());
        assert_eq!(new_new.unwrap(), PathBuf::from("new.rs"));

        let (kind_del, old_del, new_del) = classify_paths("a/old.rs", "/dev/null").expect("delete");
        assert_eq!(kind_del, PatchKind::Delete);
        assert_eq!(old_del.unwrap(), PathBuf::from("old.rs"));
        assert!(new_del.is_none());
    }

    #[test]
    fn classify_detects_renames() {
        let (kind, old, new) = classify_paths("a/src/lib.rs", "b/src/new.rs").expect("rename");
        assert_eq!(kind, PatchKind::Rename);
        assert_eq!(old.unwrap(), PathBuf::from("src/lib.rs"));
        assert_eq!(new.unwrap(), PathBuf::from("src/new.rs"));
    }
}
