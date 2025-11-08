# Safeedit QA Checklist

Use this checklist whenever you need to validate Safeedit end-to-end. Treat it as both a regression suite and a "does this feel intuitive?" sanity pass. Track progress with Markdown checkboxes (`- [ ]`) and capture follow-ups directly under the relevant items.

## 1. Environment & Baseline
- [ ] Run `cargo fmt`, `cargo clippy --all-targets --all-features`, and `cargo test` inside `safeedit/`; confirm they pass without unexplained warnings.
- [ ] Verify `safeedit cleanup --root . --dry-run` reports no stale `.bak` files or undo artifacts from prior runs.
- [ ] Ensure `$Env:RUST_LOG` is unset (clean output) unless verbose logging is required for debugging.

## 2. QA Sandbox Setup
- [ ] Create `qa_sandbox/` under the repo root with nested folders (`app/`, `app/lib/`, `docs/`, `notes/encodings/`, `logs/`).
- [ ] Populate sample files (UTF-8 unless stated otherwise):
  - `app/main.rs` with a `fn main()` that prints `hello QA`.
  - `app/lib/utils.rs` containing `pub const VERSION: &str = "0.1.0";`.
  - `docs/readme.md` with a few paragraphs plus intentional trailing spaces.
  - `notes/encodings/latin1.txt` saved in Latin-1 with accented characters.
  - `logs/huge_line.txt` holding a single 200 KB line to stress guardrails.
- [ ] Create `recipes/rename.yaml` that chains the supported batch verbs (`replace` + `normalize`) so both flows run in a single plan.
- [ ] Confirm each edited file gains a `.bak` sibling (Safeedit should create/increment backups automatically).

## 3. Review Command Coverage
- [ ] `safeedit review --target qa_sandbox/app/main.rs --head 5` shows numbered lines.
- [ ] `review --tail 5 --search "hello"` highlights matches and exits 0.
- [ ] Exercise `--lines 10-20` on a short file to confirm graceful messaging.
- [ ] Test follow mode: edit `main.rs` elsewhere while `review --follow` runs; ensure updates stream until CTRL+C.
- [ ] Review `logs/huge_line.txt` to prove long-line guard (>64 KB) blocks runaway output with a friendly warning.

## 4. Replace / Regex Editing
- [ ] Literal replace: change `hello QA` to `hello Safeedit QA`; preview diff, `--apply`, and verify `.bak` plus undo artifacts exist.
- [ ] Regex replace: `replace --regex --pattern "(0\.1\.0)" --replace "0.2.0"` across `qa_sandbox/**/*.rs` with `--expect 1`; confirm deterministic targeting.
- [ ] Negative case: misspell the pattern and ensure the tool surfaces closest-match context instead of silently succeeding.

## 5. Rename Command
- [ ] Run `rename --target qa_sandbox --from "VERSION" --to "APP_VERSION" --word-boundary --case-aware`.
- [ ] Confirm diff preview preserves case variants (for example `Version` becomes `AppVersion`).
- [ ] `safeedit log --tail 5` should list touched files and line spans.

## 6. Block Command
- [ ] Insert a block between `// BEGIN GENERATED` and `// END GENERATED`; confirm multi-line diff readability.
- [ ] Switch to `--mode replace` and ensure existing blocks are replaced, not duplicated.
- [ ] Run without markers to verify the error includes nearby context hints.

## 7. Apply Patch Workflow
- [ ] Craft a unified diff adding `qa_sandbox/docs/changelog.md` and editing `main.rs`.
- [ ] `safeedit apply --patch diff.patch` should walk through previews; force >200 diff lines (increase context) to trigger the internal viewer.
- [ ] Decline one hunk to confirm partial application works and logging reflects only applied hunks.

## 8. Normalize & Encoding Checks
- [ ] Dry-run `normalize --target qa_sandbox --scan-zero-width --scan-control` and confirm detections fire for files with anomalies.
- [ ] Fix trailing spaces in `docs/readme.md` via `--trim-trailing-space --apply`; verify backups and undo patches.
- [ ] Convert `latin1.txt` to UTF-8 using `--convert-encoding utf-8 --apply`; rerun normalize to confirm it is clean.
- [ ] Ensure suspected binary files are skipped with an explicit warning (unless an override flag is supplied).

## 9. Batch Recipes
- [ ] Execute `safeedit batch --plan qa_sandbox/recipes/rename.yaml`; confirm the `replace` and `normalize` steps each show their previews/approvals and respect global flags.
- [ ] Break the batch (invalid step) to ensure the runner reports the failing command and halts subsequent steps.

## 10. Reporting, Logging, Cleanup
- [ ] `safeedit report --since "now-1h" --format table` lists the current session's edits.
- [ ] `safeedit log --tail 10` shows recent JSON entries with commands, files, and line ranges.
- [ ] `safeedit cleanup --root qa_sandbox --dry-run` previews `.bak`/`.bakN`; run with `--apply` and confirm fresh edits recreate backups.
- [ ] Simulate many edits to verify `.safeedit/change_log.jsonl` trims back to the configured rolling window.

## 11. Large Diff Navigation Guardrails
- [ ] Produce >200 diff lines and confirm the internal viewer paginates with prompts for `more`, `approve`, `skip`.
- [ ] Edit `logs/huge_line.txt` until the diff exceeds the ~5 MB cap; the tool should stop with a friendly warning instead of freezing.
- [ ] Set `--pager never` to prove huge diffs still print inline with a truncation notice.

## 12. Undo / Recovery Drill
- [ ] Apply a change, note the undo patch, and reverse it (`patch -R` or the documented helper) to demonstrate recovery.
- [ ] Edit the same file repeatedly to confirm backups increment (`file.bak`, `file.bak2`, etc.) before cleanup.

## 13. Usability Debrief
- [ ] Record confusing UX, slow points, or missing helpers in `docs/safe_edit_tool_plan.md` (Follow-Up section).
- [ ] File TODOs or issues for any regressions discovered.
- [ ] Delete `qa_sandbox/` via a recycle-bin-safe command once testing artifacts are no longer needed.

> Tip: time each run-through. If a section consistently drags, consider automating portions or adjusting CLI ergonomics.
