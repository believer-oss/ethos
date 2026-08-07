---
name: park
description: Reset the current worktree to latest main, parking the current branch
allowed-tools: Bash, PowerShell
model: haiku
effort: low
---

# Park Worktree (Reset to Main)

Reset the current worktree to the latest `origin/main`, parking the current branch locally.

All paths are relative to the **repo root** (the directory containing the workspace `Cargo.toml`).

## Steps

1. First, check for uncommitted changes by running: `git status --porcelain`
   - If there are uncommitted changes, display them clearly to the user and **stop**. Do NOT pass `-Force`. Ask the user what they want to do (commit, stash, or discard).
2. Check for unpushed commits: `git log origin/main..HEAD --oneline`
   - If there are unpushed commits, display them and **stop**. Ask the user if they want to push first or discard.
   - Exception: if the branch's remote is gone and `git cherry origin/main HEAD` shows no `+` commits, the work was already squash-merged and is safe to discard. The script detects this case itself.
3. Only once the working tree is clean and there are no unpushed commits (or the user has confirmed), run the script from the repo root:
   - Windows: `powershell -NoProfile -ExecutionPolicy Bypass -File ".\.claude\scripts\Reset-WorktreeToMain.ps1" $ARGUMENTS`
   - macOS/Linux (PowerShell 7): `pwsh -NoProfile -File ./.claude/scripts/Reset-WorktreeToMain.ps1 $ARGUMENTS`
   - If PowerShell is unavailable, perform the equivalent steps by hand: `git fetch origin main`, then `git switch -C <parking-branch> origin/main --discard-changes` using the naming convention below.
4. Report the result — new parking branch name and current HEAD commit.

## Arguments

- `-Force` — Skip safety checks and discard all local changes (only pass if the user explicitly asks). Working-tree changes are always stashed first, and the stash ref is reported.

## Conventions

- **Parking branches**: `llm/<git-username>/<worktree-name>-parking-<YYYYMMDD>`, where `<git-username>` is the local part of `git config user.email` and `<worktree-name>` is the leaf directory of the worktree root. These branches are **local only — never push them**. They exist so git worktrees always hold a unique branch, and they only ever contain commits already in `main`.
- **Worktrees**: main checkout at `<repos>/ethos`, additional worktrees at `<repos>/ethos-worktrees/<name>` using short fixed names (`alpha`, `bravo`, `charlie`, `echo`). Because the parking branch embeds the worktree name, every worktree parks to its own branch and several can park concurrently.
- See `.claude/README.md` for the full branch/worktree conventions.

## Notes

- Worktrees do not share `target/` or `node_modules/`. After parking, a stale frontend build can linger; re-run `yarn` and `yarn package` in `core/ui` if the next task touches the UI.
