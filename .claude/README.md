# Claude Code Tooling

Optional tooling for contributors who use [Claude Code](https://claude.com/claude-code). Nothing here is required to build, test, or contribute to ethos — the normal `cargo` and `yarn` workflows are unaffected.

The tooling below is shared; everything else under `.claude/` is per-user and ignored:

```
.claude/                         # tracked:
  README.md                      #   this file
  skills/park/SKILL.md           #   /park — reset this worktree to latest origin/main
  skills/workflow/SKILL.md       #   /workflow — advance the workflow one step
  instructions/
    DoNextWorkflowStep.md        #   the 10-step workflow orchestrator
    RefineProposal.md            #   structured proposal review (Stage 2A)
    BreakDownWorkProposal.md     #   proposal -> ordered implementation stages (Stage 5)
  scripts/
    Reset-WorktreeToMain.ps1     #   the mechanics behind /park (PowerShell)

  workflow/                      # ignored: per-user workflow state, created on demand
    <work-slug>/                 #   plan docs, NNN stage files, WORKFLOW_STATE.json
    archive/<work-slug>/         #   design docs preserved after staging
  settings.local.json            # ignored: per-user settings
```

`.gitignore` ignores `.claude/*` and then re-includes only the tracked paths above, so personal settings and in-progress workflow state never land in the repository.

## Skills

| Skill | What it does |
|---|---|
| `/park` | Parks the current branch on a local-only branch and resets the worktree to latest `origin/main`. Refuses to run with uncommitted changes or unpushed commits unless `-Force`, and always stashes before a forced reset. |
| `/workflow` | Detects which of the 10 workflow steps you're on and executes the next one: plan → refine → fracture → singletonize → loss check → sequence → implement → test → stage → review gate. |

Portability: `/workflow` is plain markdown and works anywhere. `/park` shells out to a PowerShell script — it runs on Windows PowerShell 5.1+ and on macOS/Linux via [PowerShell 7](https://learn.microsoft.com/powershell/scripting/install/installing-powershell) (`pwsh -File .claude/scripts/Reset-WorktreeToMain.ps1`). The skill documents the equivalent plain-git steps, so you can also drive it by hand.

## Branch conventions

| Kind | Pattern | Pushed? |
|---|---|---|
| Parking | `llm/<username>/<worktree>-parking-<YYYYMMDD>` | Never |
| Work (no issue) | `llm/<username>/<slug>` | Yes, at staging |
| Work (with issue) | `llm/<username>/<issue-number>-<slug>` | Yes, at staging |

`<username>` is the local part of `git config user.email`. Human-authored branches in this repo use `<initials>/<slug>`; the `llm/` prefix marks agent-driven work so the two sets stay easy to tell apart.

Parking branches exist only so each worktree holds a unique branch. They contain nothing that isn't already in `main` — never push them.

## Worktree conventions

- Main checkout: your `ethos` clone, e.g. `<repos>/ethos`
- Additional worktrees as a sibling directory: `<repos>/ethos-worktrees/<name>`, using short fixed names (`alpha`, `bravo`, `charlie`, `echo`) rather than per-task names, so paths stay stable across tasks

Create one:

```bash
git -C <repos>/ethos fetch origin main
git -C <repos>/ethos worktree add <repos>/ethos-worktrees/alpha \
  -b llm/<username>/alpha-parking-<YYYYMMDD> origin/main
```

Because the parking branch embeds the worktree's directory name, every worktree parks to its own branch and several can park concurrently — `Reset-WorktreeToMain.ps1` retries on git lock contention for exactly this reason.

Per-worktree setup (worktrees share neither `target/` nor `node_modules/`):

```bash
cd core/ui && yarn && yarn package
cd friendshipper && yarn
mkdir -p friendshipper/build
```

## Git hooks (stricter than CI)

`core.hooksPath` points at `friendshipper/.husky`, so every local commit runs lint-staged (prettier + eslint `--fix`) across `core/ui` and `friendshipper`, then `cargo +nightly clippy --workspace --all-targets --all-features -- -D warnings`, then `cargo fmt --all --check`. `commit-msg` runs commitlint.

Practical implications, all encoded in the workflow's Stage 8:

- A commit is impossible until `cargo fmt --all` has been run and the nightly workspace clippy is clean — stricter than CI's per-package stable clippy, and it lints test code too.
- Needs a nightly toolchain: `rustup toolchain install nightly`.
- commitlint requires a **scope** from `core`, `friendshipper`, `misc`, and a type from `ci`, `chore`, `docs`, `feat`, `fix`, `perf`, `refactor`, `revert`, `style`, `test`.
- Never `--no-verify`.

## Workflow artifacts

Plan documents, stage files, and `WORKFLOW_STATE.json` live under `.claude/workflow/<work-slug>/`, which `.gitignore` excludes — they are **never committed**. Consequences worth remembering:

- Only source changes land on the work branch, so the squashed commit is clean by construction.
- Workflow state does not travel between machines or worktrees — one worktree owns one workflow.
- Git history can't recover a deleted plan. Stage 1 writes an immutable `00-original-plan.md` snapshot, which Stage 4 (loss checking) diffs against and Stage 8 archives.
