use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::io::{ErrorKind, Read};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use glob::glob;
use globset::{Glob, GlobSet, GlobSetBuilder};
use walkdir::{DirEntry, WalkDir};

const BINARY_CHECK_BYTES: usize = 4096;

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub path: PathBuf,
    pub metadata: FileMetadata,
}

#[derive(Debug, Clone)]
pub struct FileMetadata {
    pub len: u64,
    pub is_probably_binary: bool,
}

pub fn resolve_targets(
    explicit: &[PathBuf],
    globs: &[String],
    include_hidden: bool,
    exclude_patterns: &[String],
) -> Result<Vec<FileEntry>> {
    let exclude = build_exclude_globs(exclude_patterns)?;
    let mut entries = Vec::new();

    for path in explicit {
        append_path(path, include_hidden, exclude.as_ref(), &mut entries)
            .with_context(|| format!("processing target {}", path.display()))?;
    }

    for pattern in globs {
        let matches =
            glob(pattern).map_err(|err| anyhow!("invalid glob pattern '{pattern}': {err}"))?;
        for entry in matches {
            let path =
                entry.map_err(|err| anyhow!("error reading matches for '{pattern}': {err}"))?;
            append_path(&path, include_hidden, exclude.as_ref(), &mut entries)
                .with_context(|| format!("processing match {}", path.display()))?;
        }
    }

    if entries.is_empty() {
        if let Some(suggestion) = explicit.first().and_then(|path| suggest_path(path)) {
            bail!("no files matched; did you mean {}?", suggestion.display());
        }
        bail!("no files matched; provide --target or --glob");
    }

    dedup_by_path(&mut entries);
    Ok(entries)
}

fn append_path(
    path: &Path,
    include_hidden: bool,
    exclude: Option<&GlobSet>,
    acc: &mut Vec<FileEntry>,
) -> Result<()> {
    let canonical = canonicalize(path);
    let metadata = match fs::metadata(&canonical) {
        Ok(meta) => meta,
        Err(err) => {
            if err.kind() == ErrorKind::NotFound {
                if let Some(suggestion) = suggest_path(path) {
                    bail!(
                        "unable to read metadata for {}; did you mean {}?",
                        path.display(),
                        suggestion.display()
                    );
                }
            }
            return Err(err)
                .with_context(|| format!("unable to read metadata for {}", canonical.display()));
        }
    };

    if metadata.is_dir() {
        walk_directory(&canonical, include_hidden, exclude, acc)?;
        return Ok(());
    }

    if metadata.is_file() && !should_skip(&canonical, include_hidden, exclude) {
        acc.push(FileEntry {
            metadata: FileMetadata {
                len: metadata.len(),
                is_probably_binary: detect_binary(&canonical)?,
            },
            path: canonical,
        });
    }

    Ok(())
}

fn walk_directory(
    dir: &Path,
    include_hidden: bool,
    exclude: Option<&GlobSet>,
    acc: &mut Vec<FileEntry>,
) -> Result<()> {
    let walker = WalkDir::new(dir)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| include_hidden || !is_hidden(entry));

    for entry in walker {
        let entry = entry?;
        if entry.file_type().is_dir() {
            continue;
        }

        let path = entry.into_path();
        if should_skip(&path, include_hidden, exclude) {
            continue;
        }

        let metadata =
            fs::metadata(&path).with_context(|| format!("metadata for {}", path.display()))?;

        if metadata.is_file() {
            acc.push(FileEntry {
                metadata: FileMetadata {
                    len: metadata.len(),
                    is_probably_binary: detect_binary(&path)?,
                },
                path,
            });
        }
    }

    Ok(())
}

fn should_skip(path: &Path, include_hidden: bool, exclude: Option<&GlobSet>) -> bool {
    if !include_hidden && path_components_start_with_dot(path) {
        return true;
    }

    if let Some(set) = exclude {
        let candidate = normalize_slashes(path);
        return set.is_match(candidate.as_str());
    }

    false
}

fn is_hidden(entry: &DirEntry) -> bool {
    entry
        .file_name()
        .to_str()
        .map(|name| name.starts_with('.'))
        .unwrap_or(false)
}

fn path_components_start_with_dot(path: &Path) -> bool {
    path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .map(|segment| segment.starts_with('.'))
            .unwrap_or(false)
    })
}

fn normalize_slashes(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn canonicalize(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn dedup_by_path(entries: &mut Vec<FileEntry>) {
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    entries.dedup_by(|a, b| a.path == b.path);
}

fn detect_binary(path: &Path) -> Result<bool> {
    let mut file = fs::File::open(path)
        .with_context(|| format!("opening '{}' for binary detection", path.display()))?;
    let mut buf = [0u8; BINARY_CHECK_BYTES];
    let read = file.read(&mut buf)?;
    Ok(buf[..read].contains(&0))
}

fn build_exclude_globs(patterns: &[String]) -> Result<Option<GlobSet>> {
    if patterns.is_empty() {
        return Ok(None);
    }

    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let glob =
            Glob::new(pattern).map_err(|err| anyhow!("invalid exclude glob '{pattern}': {err}"))?;
        builder.add(glob);
    }

    builder
        .build()
        .map(Some)
        .map_err(|err| anyhow!("unable to build exclude globs: {err}"))
}

fn suggest_path(original: &Path) -> Option<PathBuf> {
    if original.as_os_str().is_empty() {
        return None;
    }

    if original.is_absolute() {
        let base = original.parent()?.to_path_buf();
        let file = PathBuf::from(original.file_name()?);
        return suggest_path_from(&base, &file);
    }
    let base = std::env::current_dir().ok()?;
    suggest_path_from(&base, original)
}

fn suggest_path_from(base: &Path, needle: &Path) -> Option<PathBuf> {
    if needle.as_os_str().is_empty() {
        return None;
    }

    let suffixes = collect_suffixes(needle);
    if suffixes.is_empty() {
        return None;
    }
    let file_names = collect_simple_names(&suffixes);

    let mut current = base.to_path_buf();
    let mut checked = HashSet::new();
    const MAX_ASCENT: usize = 64;
    for _ in 0..MAX_ASCENT {
        if let Some(hit) = try_direct_candidates(&current, &suffixes, &mut checked) {
            return Some(hit);
        }

        if let Some(hit) =
            search_sibling_directories(&current, &suffixes, &file_names, &mut checked)
        {
            return Some(hit);
        }

        if !current.pop() {
            break;
        }
    }

    None
}

fn collect_suffixes(needle: &Path) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut suffixes = Vec::new();

    push_suffix(&mut suffixes, &mut seen, needle);

    let components: Vec<PathBuf> = needle
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(PathBuf::from(name)),
            _ => None,
        })
        .collect();

    if components.len() > 1 {
        for idx in 1..components.len() {
            let suffix = components[idx..]
                .iter()
                .fold(PathBuf::new(), |mut acc, part| {
                    acc.push(part);
                    acc
                });
            push_suffix(&mut suffixes, &mut seen, &suffix);
        }
    }

    if let Some(file_name) = needle.file_name() {
        push_suffix(&mut suffixes, &mut seen, PathBuf::from(file_name));
    }

    suffixes
}

fn collect_simple_names(suffixes: &[PathBuf]) -> Vec<OsString> {
    let mut seen = HashSet::new();
    let mut names = Vec::new();
    for suffix in suffixes {
        if suffix.components().count() == 1 {
            if let Some(name) = suffix.file_name() {
                let os = name.to_os_string();
                if seen.insert(os.clone()) {
                    names.push(os);
                }
            }
        }
    }
    names
}

fn push_suffix(
    list: &mut Vec<PathBuf>,
    seen: &mut HashSet<OsString>,
    candidate: impl Into<PathBuf>,
) {
    let candidate = candidate.into();
    if candidate.as_os_str().is_empty() {
        return;
    }
    if seen.insert(candidate.as_os_str().to_os_string()) {
        list.push(candidate);
    }
}

fn try_direct_candidates(
    current: &Path,
    suffixes: &[PathBuf],
    checked: &mut HashSet<PathBuf>,
) -> Option<PathBuf> {
    for suffix in suffixes {
        let candidate = current.join(suffix);
        if let Some(hit) = check_candidate(candidate, checked) {
            return Some(hit);
        }
    }
    None
}

fn search_sibling_directories(
    current: &Path,
    suffixes: &[PathBuf],
    simple_names: &[OsString],
    checked: &mut HashSet<PathBuf>,
) -> Option<PathBuf> {
    const MAX_SIBLINGS: usize = 256;
    let iter = match fs::read_dir(current) {
        Ok(iter) => iter,
        Err(_) => return None,
    };

    for (idx, entry) in iter.flatten().enumerate() {
        if idx >= MAX_SIBLINGS {
            break;
        }

        let path = entry.path();
        if path.is_dir() {
            for suffix in suffixes {
                let candidate = path.join(suffix);
                if let Some(hit) = check_candidate(candidate, checked) {
                    return Some(hit);
                }
            }
        } else if path.is_file() {
            if let Some(name) = path.file_name() {
                if simple_names.iter().any(|target| target == name) {
                    if let Some(hit) = check_candidate(path.clone(), checked) {
                        return Some(hit);
                    }
                }
            }
        }
    }

    None
}

fn check_candidate(candidate: PathBuf, checked: &mut HashSet<PathBuf>) -> Option<PathBuf> {
    if !checked.insert(candidate.clone()) {
        return None;
    }

    if candidate.exists() {
        return Some(candidate);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn hidden_detection_basic() {
        assert!(path_components_start_with_dot(Path::new("./.git/config")));
        assert!(!path_components_start_with_dot(Path::new("src/main.rs")));
    }

    #[test]
    fn normalize_slashes_handles_backslashes() {
        assert_eq!(
            normalize_slashes(Path::new("foo\\bar\\baz.txt")),
            "foo/bar/baz.txt"
        );
    }

    #[test]
    fn dedup_removes_duplicates() {
        let mut entries = vec![
            FileEntry {
                path: PathBuf::from("a.txt"),
                metadata: FileMetadata {
                    len: 0,
                    is_probably_binary: false,
                },
            },
            FileEntry {
                path: PathBuf::from("a.txt"),
                metadata: FileMetadata {
                    len: 0,
                    is_probably_binary: false,
                },
            },
        ];

        dedup_by_path(&mut entries);
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn suggest_path_finds_parent_relative_match() {
        let temp = tempdir().expect("temp dir");
        let child = temp.path().join("child");
        std::fs::create_dir(&child).expect("child dir");
        let target = temp.path().join("target.txt");
        std::fs::write(&target, "test").expect("write target");

        let suggestion = super::suggest_path_from(&child, Path::new("target.txt"));
        assert_eq!(suggestion.unwrap(), target);
    }

    #[test]
    fn suggest_path_handles_relative_dirs() {
        let temp = tempdir().expect("temp dir");
        let child = temp.path().join("sub");
        std::fs::create_dir(&child).expect("child dir");
        let docs = temp.path().join("docs");
        std::fs::create_dir(&docs).expect("docs dir");
        let target = docs.join("file.md");
        std::fs::write(&target, "data").expect("write target");

        let suggestion = super::suggest_path_from(&child, Path::new("docs/file.md"));
        assert_eq!(suggestion.unwrap(), target);
    }

    #[test]
    fn suggest_path_scans_sibling_directories_for_file_name() {
        let temp = tempdir().expect("temp dir");
        let repo_root = temp.path().join("repo");
        let nested = repo_root.join("safeedit").join("src");
        std::fs::create_dir_all(&nested).expect("nested dir");
        let docs = repo_root.join("docs");
        std::fs::create_dir_all(&docs).expect("docs dir");
        let target = docs.join("plan.md");
        std::fs::write(&target, "plan").expect("write target");

        let suggestion = super::suggest_path_from(&nested, Path::new("plan.md"));
        assert_eq!(suggestion.unwrap(), target);
    }

    #[test]
    fn suggest_path_finds_nested_descendant_under_sibling() {
        let temp = tempdir().expect("temp dir");
        let workspace = temp.path().join("workspace");
        let nested = workspace.join("safeedit").join("src");
        std::fs::create_dir_all(&nested).expect("nested dir");
        let docs = workspace.join("docs").join("guides");
        std::fs::create_dir_all(&docs).expect("docs dir");
        let target = docs.join("plan.md");
        std::fs::write(&target, "plan").expect("write target");

        let suggestion = super::suggest_path_from(&nested, Path::new("docs/guides/plan.md"));
        assert_eq!(suggestion.unwrap(), target);
    }
}
