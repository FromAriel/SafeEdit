# Safe Edit Tool Implementation Plan

## Vision
Deliver a Windows-friendly Rust CLI that performs complex text/code edits while guaranteeing review-before-write semantics, deterministic matching, and reversible, atomic changes. The tool must feel as safe and intuitive as manually editing with `apply_patch`, but more powerful for large or multi-file refactors.

## Guiding Principles
- **Preview-first:** Every edit runs in dry mode and shows a full diff before any files are touched.
- **Deterministic targeting:** Searches are exact by default, with explicit regex/wildcard modes plus match-count guards.
- **Encoding aware:** Files are read and written using detected or user-specified encodings; previews normalize to UTF-8.
- **Atomic and reversible:** Writes happen via temp files + rename, optional `.bak` copies, and auto-generated undo patches.
- **Traceable history:** Every applied change records timestamps, commands, and line spans inside a rolling audit log so reviewers can cite edits precisely later.
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
8. **Path Resolver & Hinting:** Walks parent directories (up to the drive root) plus one level of nearby siblings to auto-suggest the most likely path when a requested target is missing.

## Command Surface (Initial)
| Command | Purpose | Key Flags |
| --- | --- | --- |
| `replace` | Replace a literal or regex match with supplied text. | `--regex`, `--literal`, `--count=N`, `--after-line`, `--encoding`, `--preview-context`, `--diff-only`, `--with-stdin`, `--with-clipboard`, `--with-here TAG` |
| `apply` | Replay unified `.patch`/`.diff` files (mods + file create/delete) through the preview + approval loop. | `--patch <file>` (repeatable), `--root <dir>`, plus global `--apply/--yes/--context/--color` |
| `block` | Insert/replace multi-line blocks anchored by sentinels. | `--start-marker`, `--end-marker`, `--mode={insert,replace}`, `--body (repeatable)/--body-file/--with-stdin/--with-clipboard/--body-here TAG` |
| `write` | Create or overwrite files (great for staging snippets) using the same diff/backups/undo pipeline. | `--path <file>`, `--body/--body-file/--with-stdin/--with-clipboard/--body-here TAG`, `--allow-overwrite`, `--line-ending {auto,lf,crlf,cr}` |
| `rename` | Rename identifiers/constants across files (case-preserving). | `--word-boundary`, `--case-aware`, `--target/--glob` |
| `review` | Inspect files safely via head/tail, arbitrary line ranges, or interactive stepping. | `--head N`, `--tail N`, `--lines 120-160`, `--search`, `--highlight` |
| `normalize` | Detect and fix encoding/mojibake/zero-width issues in text files without destructive rewrites. | `--encoding auto`, `--strip-zero-width`, `--strip-control`, `--trim-trailing-space`, `--ensure-eol`, `--scan-{encoding,zero-width,control,trailing-space}`, `--report-format {table,json}`, `--convert-encoding <target>`, `--apply` |
| `script` | Planned: run a single-file transform via embedded WASM/Lua/Python sandbox. | (planned) |
| `batch` | Execute a YAML-defined sequence of `replace`/`normalize` steps with one consolidated review (other verbs planned). | `--plan plan.yaml` |
| `report` | Summarize change log activity for CI/stand-ups. | `--since <RFC3339>`, `--format {table,json}` |
| `cleanup` | Remove stale `.bak`, `.bakN` safety copies with preview + approval. | `--root <dir>`, `--apply`, `--yes`, `--include-hidden` |

All commands accept `--dry-run` (default true), `--apply` (skip prompt for automation), `--undo-log <dir>`, `--no-backup`, and `--pager {auto,always,never}` (auto = inline until diffs exceed 200 lines, then switch to the built-in viewer).

The new `write` verb is intentionally simple: it exists so you can spin up snippet files (or regenerate them with `--allow-overwrite`) without fighting shell quoting. It uses the same diff-preview/backup/undo machinery as other commands and adds a `--line-ending` switch so you can force LF/CRLF/CR output when seeding cross-platform fixtures. Alongside it, heredoc-style flags (`--with-here TAG`, `--body-here TAG`) let you paste multi-line text directly into Safeedit, ending with the sentinel line instead of wrestling with shell escaping.

### CLI Semantics
- **Common flags**
  - `--target <path>` (repeatable): select explicit files or directories.
  - `--glob <pattern>` (repeatable): add globbed matches on top of explicit targets.
  - `--include-hidden / --exclude <glob>` to control traversal.
  - `--encoding <auto|utf-8|utf-16-le|...>` override when detection is insufficient.
- `--match-limit <n>` guard for maximum replacements, `--expect <n>` for exact matches.
- `--context <lines>` controls diff preview (default 3).
- `--pager {auto,always,never}` toggles the integrated diff viewer (auto switches to 200-line pages once a diff exceeds ~200 lines, but falls back to inline when stdin/stdout are not interactive).
- `--color {auto,always,never}` forces ANSI highlighting for diffs (auto defaults to on when stdout is a TTY).
- `--json` emits machine-readable diff metadata for automation.
- `replace --after-line <line>` skips matches whose first character is on or before the specified 1-based line, so you can leave headers/boilerplate untouched without crafting additional regex guards.
- `block` expects both markers to exist; `--mode replace` rewrites whatever sits between them, while `--mode insert` only fills the region if it was empty (otherwise it errors). Supply the replacement body via repeatable `--body` flags (each becomes its own line), `--body-file`, `--with-stdin`, or `--with-clipboard`. Newlines and indentation are inferred from the existing block so inserted text keeps the surrounding formatting.
- `rename --word-boundary` restricts matches to full identifiers; `--case-aware` makes the match case-insensitive and auto-adjusts the replacement casing (ALLCAPS, lowercase, PascalCase) to match each occurrence.
- **`review`**
  - Modes: `--head <n>` (default 40), `--tail <n>`, `--lines <start:end>`, `--around <line>:<context>`, `--follow` (stream file as it changes).
  - Search: `--search <pattern>` (literal) or `--regex` toggle; matches highlighted with ANSI colors.
  - Interactive stepping: `--step` starts a prompt-driven navigator that echoes absolute line numbers, supports `/pattern` literal or `re:` regex searches, `n/N` match hopping, numeric/`g <line>` jumps, and single-letter bookmarks so dysgraphia-friendly reviews stay fast.
  - Follow mode: `--follow` requires exactly one target file and is incompatible with `--step`; it reuses the same head/tail/around slices but refreshes whenever the file contents change until you hit Ctrl+C.
  - Output remains read-only; still passes through encoding normalization for display.
- **`normalize`**
  - Detection toggles: `--scan-encoding`, `--scan-zero-width`, `--scan-control`, `--scan-trailing-space`, `--scan-final-newline`. Use none to keep all detectors enabled; specify any to opt into just the detectors you need.
  - Fix toggles (all off unless `--apply`): `--convert-encoding <target>`, `--strip-zero-width`, `--strip-control`, `--trim-trailing-space`, `--ensure-eol`.
  - `--report-format {table,json}` chooses summary style; default prints table with severity levels.
  - Running without `--apply` only reports issues; with `--apply`, tool still shows diff and asks for confirmation.
  - Batch-safe defaults: accepts single files or entire folders but remains non-destructive until `--apply` is confirmed, making it the default "health check" pass for `.txt`, `.md`, and other text assets.

## File Lookup Ergonomics
- Primary lookup honors explicit paths/globs. When a file cannot be opened, the resolver walks parent directories from the working folder up to the drive root (for example, `J:\codextool\safeedit` → `J:\codextool` → `J:\`), checking each level for close matches.
- At every level the resolver also inspects sibling directories one hop away (to catch cases like `docs\` vs. `safeedit\docs\`) so we surface likely matches without spidering the entire drive.
- The user-facing error reads `File <name> not found. Did you mean <Drive:\path\file>?` and can return multiple ranked candidates when ambiguity remains.
- Hints feed back into the CLI so a rerun can be auto-corrected (e.g., `safeedit replace --target docs\safe_edit_tool_plan.md` even when invoked from `safeedit\`).

## Change Tracking & Logging
- All commands share `.safeedit/change_log.jsonl` as a lightweight, append-only queue capped at ~500 entries (configurable). Older entries roll off automatically to keep the file tiny.
- Each entry captures timestamp, verb, arguments, file path, applied/skipped status, a hash of the diff, and the start/end line numbers that changed so reviewers can tie logs back to source quickly.
- `safeedit log --tail N` (and future `--search`, `--since`) exposes the queue without cracking open JSON.
- `safeedit report --since <ts> --format {table,json}` summarizes those entries for CI, status emails, or reviewers.

## Workflow
1. User issues command (e.g., `safeedit replace --glob "src/**/*.rs" --literal "foo" --with "bar"`).
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
- **Backup hygiene:** Once changes are verified, run `safeedit cleanup --root <dir>` (preview first, then `--apply`) so `.bak`/`.bakN` files do not accumulate indefinitely.
- **Logging:** Append JSONL entries capturing command, arguments, files touched, diff hash, success/failure.
- **Normalize guardrails:** Text-only by default: suspected binary files are skipped with a warning, and the `--scan-*` flags only enable the detectors you explicitly request (otherwise all detectors run).
- **Diff viewer guardrails:** Once a diff exceeds ~200 lines (or when `--pager always`), previews run through the internal pager that shows 200-line pages, caps captured output at ~5 MB/5k lines, and truncates any single diff line beyond 64 KB with a reminder to rerun with `--pager never` + narrower targets for pathological files (e.g., 500 MB single-line JSON blobs).
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
- `diffy` for parsing/applying unified patches inside `safeedit apply`.
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
- Encoding strategy module validates overrides up front and auto-detects via BOM + UTF-8 sniff + `chardetng`, exposing decode helpers to downstream commands.
- `review` command prototype loads resolved files, decodes them safely, and prints head/tail/range/around slices with optional literal/regex highlights (defaulting to the first 40 lines if no slice specified). Binary-suspect files are skipped with a warning, and follow mode currently falls back to static preview.
- Transform pipeline now powers `replace`: for each resolved file we decode once, apply literal/regex replacements with optional capture expansion, enforce `--count/--expect`, and render a Myers diff preview via the `similar` crate. All operations remain dry-run with per-file summaries until we add apply/confirm UX.
- `replace` now supports interactive approvals: run with `--apply` to approve each diff (`y/n/a/q`), still defaulting to dry-run previews otherwise. Writes reuse the source encoding and warn about lossy conversions.
- `--yes` auto-approve flag applies all diffs without prompting (useful for CI), still recording each action in the change log.
- Diff previews are now colorized (green inserts, red deletes) and respect `--color {auto,always,never}` so big patches are easier to scan inline.
- Undo archives: passing `--undo-log <dir>` now writes a unified `.patch` per applied file (new ➜ old) so we can roll back later without relying on Git.
- Missing-path ergonomics: when a requested file isn't found, the tool now walks every ancestor plus the drive root, tries each suffix of the requested path (not just the file name), and inspects immediate child directories so we can suggest paths like `..\docs\guides\plan.md` even when the command ran out of `safeedit\src\`.
- Zero-match diagnostics: literal/regex replacements that find no hits now surface the top three fuzzy matches, including line/column numbers, snippets, and inline diff markers so typos are easy to spot and correct.
- Rolling change log: every replace run appends JSONL entries to `.safeedit/change_log.jsonl` capturing timestamp, command, path, action, line summary, and the specific line spans impacted (distinguishing modified vs. added). The log retains the latest ~500 entries for lightweight code-review breadcrumbs.
- `review --step` engages an interactive line navigator (Enter/`j`=next line, `b`/`p`/`k`=previous, `g`/`G` jump to head/tail, `/pattern` sets a literal/`re:` regex search, `n`/`N` hop between matches, `m` drops a bookmark, `'` jumps back, numeric or `g <line>` jumps work as shortcuts, `q` exits) so dysgraphia-friendly walkthroughs never require leaving the terminal or losing context.
- Diff viewer automatically switches to an internal pager when diffs exceed ~200 lines, showing 200-line chunks with the same navigation keys as `review --step`, while guarding against runaway blobs with 5 MB/5k-line buffers and 64 KB-per-line caps.
- Replace input ergonomics: `--diff-only` forces preview-only runs (even with `--apply`), while `--with-stdin` and `--with-clipboard` let us feed large replacements without wrestling with shell quoting.
- Auto-approve summaries: replace/normalize now finish with a concise applied/skipped/dry-run/no-op report so unattended runs (e.g., `--yes`) still show what happened.
- `safeedit report --since <ts>` condenses change-log entries into per-command/action summaries (table or JSON) so CI jobs or stand-ups can cite what happened without tailing raw logs.
- Batch runner: `safeedit batch --plan plan.yml` loads YAML/JSON recipes (with per-step `common` overrides) and replays `replace`/`normalize` steps through the usual preview + approval loop, so multi-command refactors stay reviewable without ad-hoc scripts.
- `safeedit report --since <ts>` condenses change-log entries into per-command/action summaries (table or JSON) so CI jobs or stand-ups can cite what happened without tailing raw logs.
- `safeedit log --tail N` prints the most recent change-log entries (timestamp, command, action, line summary, path) so reviewers can quickly audit what changed without opening the JSONL file.
- `safeedit apply --patch` ingests unified `.patch`/`.diff` files per target file, previews via the existing diff UI, honors `--apply/--yes`, records undo patches/log entries, and now supports same-path modifications plus file adds/deletes/renames.
- Normalize command now supports detection toggles (`--scan-*`), `--report-format json` for machine-readable health checks, and `--convert-encoding <label>` so apply runs can transcode and clean in one pass; table output remains the default for interactive use.
- Block command enforces sentinel-anchored replacements: both markers must exist, `--mode insert` refuses to overwrite non-empty regions, bodies can flow from literals/files/stdin/clipboard, repeatable `--body` literals make quick multi-line edits easy, and Safeedit mirrors the original line endings/indentation so replacements stay aligned.
- Rename command now rewrites identifiers across files with optional word-boundary enforcement and case-aware replacements that adapt the new text (lowercase, uppercase, capitalized) to match each occurrence.
- Unit tests cover the new helpers (file dedupe/normalization, encoding detection, review parsing/highlighting).

### Upcoming polish
- Diff UX polish: the internal pager now buffers up to ~5 MB/5k lines, enforces a 64 KB-per-line cap, and paginates in 200-line chunks (Enter/`n`, `p`, `g <line>`, `h`/`t`, `q`). Next steps are side-by-side diff rendering, optional HTML/export outputs, and richer color themes for large refactors.
- Review ergonomics: extend `--step` with multi-bookmarks, search result listings, and copy-to-clipboard/export commands so longer reviews remain fast even when scanning thousands of lines.

## Outstanding Feature Checklist
| Area | Feature | Status | Notes |
| --- | --- | --- | --- |
| Path resolution | Ancestor + sibling search with "did you mean" hints | Done | Resolver now tries every ancestor, drive-root suffixes, and child directories (including nested suffix matches) before emitting suggestions. |
| Matching feedback | Multi-suggestion zero-match hints | Done | `replace` now reports up to three nearest matches with snippets and inline diff markers. |
| Input ergonomics | `--diff-only`, `--stdin`, `--clipboard` for `replace` | Done | Diff-only previews plus stdin/clipboard replacement sources keep large edits safe without shell acrobatics. |
| Normalize | Encoding conversion + JSON reporting | Done | Normalize now emits table/JSON summaries, honors `--scan-*`, and can rewrite using a target encoding. |
| Batch/recipes | YAML/JSON batch runner with consolidated approval | Done | `safeedit batch --plan` replays replace/normalize steps with shared previews + approvals. |
| Undo assets | `.undo.patch` emission + auto-approve summary | Done | `--undo-log` drops per-file patches and each command prints applied/skipped/dry-run/no-op counts. |
| Reporting | `safeedit report --since <ts>` | Done | Table/JSON summaries of change-log entries for CI, stand-ups, or auditors. |
| Diff UX | Color, side-by-side, pager integration | In progress | ANSI colors plus the built-in pager now ship; side-by-side layouts and HTML/export outputs remain on deck. |
| Logging | Line-number aware log entries w/ rolling retention | Done | JSONL entries now include structured spans (modified vs added) alongside summaries, still capped at ~500 rows. |
| Review UX | Interactive stepping (`--step`, `--line`) | Done | `safeedit review --step` now navigates line-by-line with next/prev/head/tail, `/pattern` searches, `n/N` match hopping, and single-letter bookmarks. |
| Patch ingest | Apply external `.patch`/`.diff` files | Done | `safeedit apply --patch` parses unified diffs with diffy, previews each file, honors `--apply/--yes`, logs every action, and now handles same-path edits plus file adds/deletes/renames. |

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

## Follow-Up Issues (Nov 2025 Review)
- **Batch runner/docs mismatch** (`safeedit/src/batch.rs:10-78`, `qa_sandbox/recipes/rename.yaml:1`): the CLI only understands `replace`/`normalize`, yet the plan and QA recipe reference `review`/`rename` steps. Decide whether to expand the enum or tighten documentation/examples so recipes reflect the actual parser.
- **Planned commands:** `script` remains aspirational—either ship a concrete WASM/Lua/Python sandbox with preview-before-write semantics or keep it clearly labeled as "planned" so expectations stay aligned.

This plan keeps the editing experience transparent, reviewable, and recoverable while giving us headroom to automate ever more complex transformations safely.
