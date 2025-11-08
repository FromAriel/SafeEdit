# Safeedit

Safeedit is a Windows-friendly Rust CLI for applying complex text/code edits while guaranteeing preview-before-write semantics, deterministic targeting, and reversible, atomic changes. It’s designed as a safer, more powerful alternative to manual `apply_patch` workflows—especially for large refactors, multi-file replacements, or encoding cleanup jobs.

## Why Safeedit?
- **Preview-first:** Every edit renders a diff before touching disk. You can skip, apply once, or “apply all”.
- **Deterministic targeting:** Literal, regex, and block operations include match guards (`--expect`, `--after-line`, etc.) plus helpful suggestions when nothing matches.
- **Encoding aware:** Detects BOMs/encodings, preserves original line endings, and can convert troublesome files to UTF-8.
- **Atomic & reversible:** Writes happen via temp files, `.bak` rotation (`file.bak`, `file.bak1`, …), and optional undo patches (`--undo-log`), so every change is recoverable.
- **Traceable history:** Each action logs JSON lines with timestamps, commands, files, and line spans for later review (`safeedit log`, `safeedit report`).
- **Windows ergonomics:** Works out-of-the-box on PowerShell, guards against runaway output (200-line diff pages, 5 MB cap, 64 KB/line), and skips suspected binary files unless you explicitly opt in.

## Feature Highlights
| Command | Purpose | Example |
| --- | --- | --- |
| `replace` | Literal/regex replacements with diff previews and match guards. | `safeedit replace --target src --literal --pattern "foo" --with "bar" --expect 3` |
| `rename` | Case-aware identifier renames with word-boundary controls. | `safeedit rename --target app --from VERSION --to APP_VERSION --word-boundary --case-aware` |
| `block` | Insert/replace multi-line regions bounded by markers. | `safeedit block --target file.rs --start-marker "// BEGIN" --end-marker "// END" --mode replace --body-file new_block.txt` |
| `apply` | Replay unified `.patch`/`.diff` files (modify/create/delete/rename) through the preview/approval pipeline while preserving original newline styles. | `safeedit apply --patch changes.diff --apply` |
| `review` | Safe file viewing: `--head`, `--tail`, `--lines`, `--search`, `--step`, or long-running `--follow`. Built-in pager kicks in past ~200 diff lines. | `safeedit review --target app/main.rs --head 20 --search todo` |
| `normalize` | Detect/repair zero-width chars, control chars, trailing spaces, final newlines, encoding mojibake, and convert encodings. | `safeedit normalize --target docs --trim-trailing-space --ensure-eol --convert-encoding utf-8 --apply` |
| `batch` | Execute YAML/JSON “recipes” that chain supported verbs (`replace`, `normalize`) with shared review logging. | `safeedit batch qa_sandbox/recipes/rename.yaml --apply --yes` |
| `report` | Summaries of logged edits for CI/standups (`table` or `json`). | `safeedit report --since 2025-11-08T14:00:00-07:00` |
| `log` | Tail the rolling `.safeedit/change_log.jsonl` audit trail. | `safeedit log --tail 20` |
| `cleanup` | Find/remove `.bak`/`.bakN` safety files once you’re confident in edits. | `safeedit cleanup --root . --apply --yes` |

Additional niceties:
- `--pager {auto,always,never}` toggles the internal diff viewer (auto pages when diffs exceed ~200 lines but stay under the 5 MB/64 KB guardrails).
- `--color` and `--json` adjust output style for automation.
- Path auto-resolution walks up from the current directory to suggest likely files when a target can’t be found.
- `qa_sandbox/` plus `docs/qa_testing_checklist.md` define a repeatable regression suite covering review, replace, rename, block, apply, normalize, batch, report/log, and cleanup scenarios.

## Installation
1. **Prerequisites**
   - Rust toolchain (1.75+ recommended) with Cargo.
   - Windows 10/11 with PowerShell (Unix shells work too, but the guardrails are tuned for Windows consoles).
2. **Clone & build**
   ```powershell
   git clone <repo-url> safeedit
   cd safeedit\safeedit
   cargo build --release
   ```
   The binary will be at `safeedit\target\release\safeedit.exe`.
3. **(Optional) Add to PATH**
   ```powershell
   $env:Path += ";$PWD\target\release"
   ```

## Quick Start
```powershell
# Preview first replacement (no files touched)
safeedit replace --target src/lib.rs --literal --pattern "hello" --with "hello Safeedit"

# Apply the change after review
safeedit replace --target src/lib.rs --literal --pattern "hello" --with "hello Safeedit" --apply

# Rename identifiers across the project, preserving case
safeedit rename --target app --from VERSION --to APP_VERSION --word-boundary --case-aware --apply

# Apply a git-style patch (modify + create files)
safeedit apply --patch fix.diff --apply

# Normalize documentation: trim trailing spaces, ensure final newline, convert encoding
safeedit normalize --target docs --trim-trailing-space --ensure-eol --convert-encoding utf-8 --apply

# Run a batch recipe that chains replace + normalize steps
safeedit batch qa_sandbox/recipes/rename.yaml --apply --yes

# Inspect recent edits
safeedit report --since 2025-11-08T15:00:00Z --format table
safeedit log --tail 10
```

## Safety Mechanisms
- **Diff previews everywhere** with `apply`/`skip` prompts and `--yes/--auto-apply` overrides for CI.
- **Atomic writes** via temp files + rename; backups rotate (`.bak`, `.bak1`, …) unless `--no-backup` is used.
- **Undo artifacts**: `--undo-log <dir>` drops reverse patches you can replay with `patch -R`.
- **Encoding fidelity**: detection respects BOM > chardet > UTF-8 fallback; newline preservation ensures CRLF files remain CRLF even after patches.
- **Guardrails**: 200-line diff window, 5 MB total diff output ceiling, 64 KB per line, binary-file detection, and follow-mode safeguards.
- **Logging & reporting**: every command writes JSONL entries consumed by `safeedit report` / `safeedit log`.

## QA & Documentation
- `docs/safe_edit_tool_plan.md` — vision, architecture, and roadmap (including planned commands such as `script`).
- `docs/qa_testing_checklist.md` — a reproducible end-to-end validation script (environment prep, sandbox creation, command coverage, cleanup, usability notes).
- `qa_sandbox/` — sample workspace with nested folders, encoding edge cases, and batch recipes for testing.

## Roadmap
- Diff UX polish (side-by-side viewer, HTML export).
- Review ergonomics (`--follow` enhancements, bookmark/search helpers).
- Batch-runner verb expansion and optional scripted transforms (`script` command).
- Additional normalize/report output formats driven by user feedback.

Contributions and experiments are welcome—just follow the QA checklist and keep the safety guarantees front-and-center. Happy editing!
