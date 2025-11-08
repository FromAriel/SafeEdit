# Agent Workflow

Use this guide every time you work in a project so our files stay safe and easy to recover, regardless of the tech stack.

## 0. Quick Checklist (in order)
1. **Back up**: copy the file you plan to edit to `filename.ext.bak` (if one exists, increment to `.bak2`, `.bak3`, etc.).
2. **Edit safely**: prefer `apply_patch` so diffs stay reviewable; other tooling is fine, but double-check the result.
3. **Verify**: run the relevant build or test command. Fix errors and critical warnings. Incidental warnings can wait, but if they accumulate schedule a warning-fix pass.
4. **Report**: mention the verification command and outcome in your update.
5. **Clean up**: only after verification succeeds, delete backups with a safe-delete command so they land in the recycle bin/trash (for example `Remove-ItemSafely path\to\file.bak` on Windows or `trash-put path/to/file.bak` on Linux).

## 1. Back up before you edit
- Before touching a file, create a sibling backup with the `.bak` suffix. Use `.bak2`, `.bak3`, etc. if another backup already exists.
- After the change is verified (builds, behaviour validated), remove the backup via your platform's safe-delete tool so the file goes to the recycle bin/trash instead of being permanently deleted. If no such tool exists, move the backup into a `backup/` folder that is committed or otherwise easy to restore from.

## 2. Prefer safe editing tools
- Use `apply_patch` for most text and code edits because it keeps diffs small and reviewable.
- It is fine to use PowerShell, Python, or other scripts when they are the best fit (for example, wide renames); just validate the result because these paths are easier to misapply.

## 3. If something goes wrong, stop and report
- Corruption, regressions, or accidental destruction of in-progress work: stop immediately.
- Explain what you were doing, what broke, and outline recovery options (restore the `.bak`, pull from the recycle bin, fall back to an older Git revision, etc.) and wait for direction.
- Minor typos or obvious compile errors with intact structure do not require escalation; fix and continue.

## 4. Validation
- After edits, run the build, lint, and/or test commands that matter for the language(s) in this repository.
- Keep a short list of canonical commands (for example `npm run lint`, `cargo fmt`, `pytest`) in `PROJECT_NOTES.md` or the README so every agent runs the same checks.
- Only remove backups once you have verified the results (or after you have reverted when asked).

## 5. Communication
- Always mention the checks you ran (build, test, lint) in your update.
- Capture any TODOs or follow-up tasks so the next session picks up smoothly.

## Repository-Specific Notes
- Codebase is a Rust workspace rooted at `safeedit/` (Rust 2021 edition, stable toolchain). Documentation and planning artifacts live under `docs/`, while `.safeedit/` stores change logs and undo artifacts (ignored by Git).
- Canonical verification commands (run inside `safeedit/`): `cargo fmt`, `cargo clippy --all-targets --all-features`, and `cargo test`. Run all three before marking a task complete; note the results in your update.
- The `safeedit` binary is our primary editing tool—prefer invoking `cargo run -- <subcommand>` instead of ad-hoc scripts so we keep approvals, logging, and undo files consistent.
- Backups: every write path produces `.bak`, `.bak2`, etc. Keep them until verification/tests pass, then prune safely via `safeedit cleanup --root .` (preview first; use `--apply` only when ready).
- Change tracking: `safeedit log --tail N` and `safeedit report --since <ts>` summarize the rolling JSONL log in `.safeedit/change_log.jsonl`; cite these when explaining edits to teammates.

## Environment Notes
- Default sessions run on Windows 10 inside Windows Terminal, which launches PowerShell for every shell command. If your environment differs, add a short note here so future agents know which shell/os assumptions are safe.
- Quoting rules:
  * Wrap scripts or paths in double quotes; PowerShell treats single quotes as literal strings (no interpolation).
  * Escape double quotes inside command strings with `` `" `` or build the string via concatenation.
  * Use backticks (`` ` ``) to escape special characters, or prefer double-quoted strings and the `--%` stop-parsing token when passing raw arguments.
- Paths use backslashes by default; if you copy or paste from POSIX examples, adjust them to `C:\style\paths`.
- Executable lookup follows PowerShell rules, so call tools with explicit extensions when needed (for example, `python.exe`).
- Soft deletes: prefer commands that route through the OS recycle bin/trash (for example `Remove-ItemSafely`, `trash-put`, or Finder's `.Trash`). Avoid destructive flags like `-Force`, `rm -rf`, or permanent delete switches unless explicitly requested.
