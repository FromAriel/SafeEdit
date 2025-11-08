use anyhow::Result;
use similar::{ChangeTag, TextDiff};

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
