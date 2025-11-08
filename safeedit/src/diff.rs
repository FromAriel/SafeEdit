use anyhow::Result;
use similar::{ChangeTag, DiffTag, TextDiff};

pub fn print_diff(old: &str, new: &str, context: usize) -> Result<()> {
    let diff = TextDiff::configure()
        .algorithm(similar::Algorithm::Myers)
        .diff_lines(old, new);

    for (idx, group) in diff.grouped_ops(context).iter().enumerate() {
        if idx > 0 {
            println!("...");
        }
        for op in group {
            for change in diff.iter_changes(op) {
                match change.tag() {
                    ChangeTag::Delete => print!("- "),
                    ChangeTag::Insert => print!("+ "),
                    ChangeTag::Equal => print!("  "),
                }
                print!("{change}");
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
