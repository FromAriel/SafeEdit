# Safe Edit Tool Implementation Plan

## Vision
Deliver a Windows-friendly Rust CLI that performs complex text/code edits while guaranteeing review-before-write semantics, deterministic matching, and reversible, atomic changes. The tool must feel as safe and intuitive as manually editing with `apply_patch`, but more powerful for large or multi-file refactors.

## Guiding Principles
- **Preview-first:** Every edit runs in dry mode and shows a full diff before any files are touched.
- **Deterministic targeting:** Searches are exact by default, with explicit regex/wildcard modes plus match-count guards.
- **Encoding aware:** Files are read and written using detected or user-specified encodings; previews normalize to UTF-8.
- **Atomic and reversible:** Writes happen via temp files + rename, optional `.bak` copies, and auto-generated undo patches.
- **Actionable failures:** When a pattern isn't found, provide the closest matches and context, never silent no-ops.
- **Extensible:** Support simple replacements now, but leave hooks for scripted transforms and batch recipes later.

## High-Level Architecture
1. **Command Parser (clap):** Parses verbs like `replace`, `rename`, `script`, global flags (encoding override, include/exclude globs, dry-run).
2. **File Scanner:** Resolves file sets (respecting glob filters, size limits, binary detection).
3. **Match Engine:** Performs literal/regex/pattern searches, tracks offsets, validates match counts, and captures context for reporting.
4. **Diff Generator:** Uses `similar`/`difflib`-like crate to build unified or side-by-side diffs for preview (colorized via `nu-ansi-term`).
5. **Approval Loop:** Prompts user (and surfaces JSON for automation if needed) to `apply`, `skip`, or `modify` per file or batch.
6. **Write Pipeline:** Creates temp file, applies transformation, validates checksum/size expectations, then atomically swaps and records `.undo.patch`.
7. **Audit Logger:** Appends command metadata, timestamps, and diff hashes to a log for traceability.

## Command Surface (Initial)
| Command | Purpose | Key Flags |
| --- | --- | --- |
| `replace` | Replace a literal or regex match with supplied text. | `--regex`, `--literal`, `--count=N`, `--after-line`, `--encoding`, `--preview-context` |
| `block` | Insert/replace multi-line blocks anchored by sentinels. | `--start-marker`, `--end-marker`, `--mode={insert,replace}` |
| `rename` | Rename identifiers/constants across files (case-preserving). | `--word-boundary`, `--case-aware`, `--files` |
| `review` | Inspect files safely via head/tail, arbitrary line ranges, or interactive stepping. | `--head N`, `--tail N`, `--lines 120-160`, `--search`, `--highlight` |
| `normalize` | Detect and fix encoding/mojibake/zero-width issues in text files without destructive rewrites. | `--encoding auto`, `--strip-zero-width`, `--report-only`, `--apply` |
| `script` | Run a single-file transform via embedded WASM/Lua/Python sandbox (phase 2). | `--script-file`, `--arg` |
| `batch` | Execute a YAML-defined sequence of commands with one consolidated review. | `--plan plan.yaml` |

All commands accept `--dry-run` (default true), `--apply` (skip prompt for automation), `--undo-log <dir>`, `--no-backup`, and `--pager`.

### CLI Semantics
- **Common flags**
  - `--files <globs>` (repeatable): target set; defaults to explicit positional file args.
  - `--include-hidden / --exclude <glob>` to control traversal.
  - `--encoding <auto|utf-8|utf-16-le|...>` override when detection is insufficient.
  - `--match-limit <n>` guard for maximum replacements, `--expect <n>` for exact matches.
  - `--context <lines>` controls diff preview (default 3).
  - `--pager <less|more|none>` chooses preview transport.
  - `--json` emits machine-readable diff metadata for automation.
- **`review`**
  - Modes: `--head <n>` (default 40), `--tail <n>`, `--lines <start:end>`, `--around <line>:<context>`, `--follow` (stream file as it changes).
  - Search: `--search <pattern>` (literal) or `--regex` toggle; matches highlighted with ANSI colors.
  - Output remains read-only; still passes through encoding normalization for display.
- **`normalize`**
  - Detection toggles: `--scan-encoding`, `--scan-zero-width`, `--scan-control`, `--scan-trailing-space`.
  - Fix toggles (all off unless `--apply`): `--convert-encoding <target>`, `--strip-zero-width`, `--strip-control`, `--trim-trailing-space`, `--ensure-eol`.
  - `--report-format {table,json}` chooses summary style; default prints table with severity levels.
  - Running without `--apply` only reports issues; with `--apply`, tool still shows diff and asks for confirmation.

## Workflow
1. User issues command (e.g., `safeedit replace --files src/**/*.rs --literal "foo" --with "bar"`).
2. Tool expands file list, skipping suspected binaries unless `--force-binary`.
3. For each file, the match engine locates targets; if none, it reports near matches (top-N Levenshtein hits with +/-5 lines context).
4. Diff generator builds unified diff per file plus summary stats (# files, hunks, bytes).
5. Preview printed to console/pager, user chooses:
   - `a` apply all shown changes,
   - `y/n` per file,
   - `e` edit command (returns to prompt without touching files).
6. On apply, tool writes via temp file + `fsync`, optional `.bak` copy, records undo patch, updates audit log.
7. Final status includes reminder to run `cargo fmt`, `cargo clippy --all-targets --all-features`, `cargo test` (or repo-specific commands).

The `review` command shares the preview loop but never writes; it simply paginates head/tail/line-range views (with optional regex highlighting) so large files can be inspected safely before crafting edits.

`normalize` runs in two phases: (1) analysis that reports suspected encoding anomalies, mojibake spans, zero-width or control characters, and trailing space issues; (2) optional apply step that rewrites via temp files only after a diff preview confirms the cleanup. It can target single files or batches and supports `--report-only` for non-destructive audits.

## Safety + Feedback Mechanisms
- **Miss diagnostics:** When zero matches, show suggestions like  
  `No exact match for "FooBar"; closest occurrences:` with line numbers/diff.
- **Match guards:** `--expect <n>` fails if match count differs; `--max <n>` prevents runaway replacements.
- **Encoding handling:** Auto-detect via `chardetng` or BOM; manual override; diff output always UTF-8 to avoid mojibake.
- **UNDO artifacts:** Save `*.undo.patch` (git-style) plus manifest referencing original files & checksums.
- **Backups:** Optional `.bak` or timestamped copies; default on for non-git repos, configurable via `~/.safeedit.toml`.
- **Logging:** Append JSONL entries capturing command, arguments, files touched, diff hash, success/failure.
- **Review mode guarantees:** `review` never writes, always respects encoding detection, and supports piping output to other tools, ensuring inspection actions are side-effect free.
- **Normalization guardrails:** `normalize` defaults to report mode, requires explicit confirmation before altering content, and records both detected issues and applied fixes in the audit log.

## Windows-Oriented Features
- Native path handling via `std::path::PathBuf`.
- Color output respecting `NO_COLOR` and Windows Terminal ANSI support.
- Integration with `less`/`more` fallback; can spawn `code --diff` if requested.
- Safe delete hook to call `Remove-ItemSafely` after successful verification if backups should be cleaned automatically.

## Dependencies (planned)
- `clap` for CLI parsing.
- `globset` + `ignore` for file selection.
- `encoding_rs` / `chardetng` for encoding detection.
- `similar` or `dissimilar` for diff generation.
- `serde` + `serde_json` for logging and batch plans.
- `anyhow` / `miette` for rich error reporting.

## Implementation Checklist
1. **Scaffold project**
   - `cargo new safeedit`
   - Set up `cargo fmt`, `cargo clippy --all-targets --all-features`, `cargo test` in CI/local tasks.
2. **Config + CLI**
   - Define global flags and core verbs via `clap`.
   - Load layered config (`$PROJECT/.safeedit.toml`, user config).
3. **File discovery + encoding layer**
   - Implement glob filtering, binary detection, encoding detection/override.
4. **Match + transform engine**
   - Literal + regex replace with match-count guards.
   - Block sentinel operations.
   - Identifier-aware rename groundwork.
5. **Diff + preview UX**
   - Generate unified/side-by-side diff.
   - Interactive prompt loop with per-file apply/skip.
   - Integrate `review` mode to reuse diff/preview UI without touching files.
6. **Write pipeline**
   - Temp-file writes, `.bak` handling, undo patch generation.
7. **Feedback + logging**
   - Miss diagnostics, summary output, JSONL audit log.
8. **Normalization utility**
   - Detector for encodings/mojibake/zero-width characters.
   - Safe rewrite pipeline with diff preview and batch support.
9. **Advanced ops**
   - Scripting hook scaffolding, recipe runner enhancements.
10. **Documentation**
   - User guide, command reference, recipes.
11. **Testing & QA**
    - Unit tests for matchers, encoding conversions.
    - Integration tests simulating edits on fixtures.
    - Manual validation on Windows Terminal, ensuring diff colors, pager integration, and undo flow behave as expected.

## Prototype Status
- Basic CLI scaffold in `safeedit/` now parses all verbs and prints structured summaries.
- File discovery layer honors `--target`, `--glob`, hidden files, excludes, and does lightweight binary detection before edits.
- Encoding strategy module validates overrides up front and auto-detects via BOM → UTF-8 check → `chardetng`, exposing decode helpers to downstream commands.
- `review` command prototype loads resolved files, decodes them safely, and prints head/tail/range/around slices with optional literal/regex highlights (defaulting to the first 40 lines if no slice specified). Binary-suspect files are skipped with a warning, and follow mode currently falls back to static preview.
- Transform pipeline now powers `replace`: for each resolved file we decode once, apply literal/regex replacements with optional capture expansion, enforce `--count/--expect`, and render a Myers diff preview via the `similar` crate. All operations remain dry-run with per-file summaries until we add apply/confirm UX.
- `replace` now supports interactive approvals: run with `--apply` to approve each diff (`y/n/a/q`), still defaulting to dry-run previews otherwise. Writes reuse the source encoding and warn about lossy conversions.
- Missing-path ergonomics: when a requested file isn’t found, the tool now scans up from the current directory to suggest likely matches (e.g., “did you mean `J:\codextool\docs\...`?”) before bailing.
- Unit tests cover the new helpers (file dedupe/normalization, encoding detection, review parsing/highlighting).

### Upcoming polish (from dogfooding)
- Deeper path ergonomics: allow targeting files relative to the workspace root even when `safeedit` runs inside the crate (e.g., auto-prepend `..` when obvious).
- Missing-match diagnostics: when replacements find zero hits, display the nearest occurrences with context, just like the plan originally called for.
- Convenience flags for long replacements (`--diff-only`, `--stdin`/`--clipboard` sources) to avoid pasting giant strings into the CLI.

## Build & Verification Commands
Run these from `safeedit/`:
1. `cargo fmt`
2. `cargo clippy --all-targets --all-features`
3. `cargo test`
Latest verification (2025-11-07, 16:00 UTC): all three commands passed successfully after enabling the replace pipeline.
Enable `RUST_LOG=safeedit=debug` when we add logging, and consider `cargo nextest`/`cargo tarpaulin` later for faster test cycles and coverage.

## Future Enhancements
- Batch plan authoring UI (YAML/JSON templates).
- Language server hints for symbol renames.
- Pluggable diff viewers (e.g., HTML export).
- VS Code/Editor integration that shells out to `safeedit`.

This plan keeps the editing experience transparent, reviewable, and recoverable while giving us headroom to automate ever more complex transformations safely.
