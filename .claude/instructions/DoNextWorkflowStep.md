<!-- claude-hint:
model: opus
effort: max
rationale: Creation Workflow orchestrator; stage decisions require depth reasoning; delegates heavy stages to subagents (which have their own hints)
-->

Do not use instructions from this file unless asked.

# Do Next Workflow Step

This instruction is the orchestrator for the **Creation Workflow** — a multi-stage process that takes a feature idea from initial concept through planning, refinement, implementation, testing, and staging for PR. An instance of Claude Code reading this document should be able to detect the current stage and execute the next step, or start from scratch if no workflow is in progress.

## Overview

The Creation Workflow has 10 steps across 9 numbered stages:

```
1. Plan              — Clarify intent, research codebase, produce a work proposal
2A. Refine           — Iterative improvement with user feedback
2B. Fracture         — Break plan into parallel-editable documents
3. Singletonize      — Harden each document to stand alone without ambiguity
4. Loss Checking     — Verify nothing was dropped between original plan and final documents
5. Sequencing        — Turn the plan into ordered implementation stages (skeleton → test → implement → ...)
6. Implementing      — Execute the sequenced stages as commits
7. Testing           — Verify fmt/clippy/tests/lint pass on a rebased branch, debug failures
8. Staging           — Archive plan docs, rebase on main, run lint/format, squash, write the final commit, push, create PR
9. Review Gate       — Human reviews and approves the PR; PR squash-merges
```

## Where Workflow Artifacts Live

**All workflow artifacts are local-only and never committed.** `.gitignore` ignores `.claude/*` and re-includes only the shared tooling (`README.md`, `instructions/`, `scripts/`, `skills/`), so `.claude/workflow/` — every plan document, stage file, and state file — stays out of the repository.

```
.claude/workflow/<work-slug>/          # active workflow: plan docs, stage files, WORKFLOW_STATE.json
.claude/workflow/archive/<work-slug>/  # design docs preserved after staging
```

Consequences that differ from a workflow that commits its plans:

- Plan documents are **not** committed to the work branch, so there is no "delete the plan docs before squashing" step. Instead, Stage 8 **verifies** that no workflow artifacts leaked into the diff.
- Loss checking (Stage 4) cannot diff plan commits from git history. Stage 1 therefore writes an immutable snapshot, `00-original-plan.md`, that Stage 4 compares against.
- Workflow state does not travel between machines or worktrees. One worktree owns one workflow.

## Stage Model/Effort Hints

When executing a stage that spawns subagents, pass these model/effort overrides to `Agent(...)` calls. The table below captures the default policy; stage sections (1-9) may override per case.

| Stage | Recommended model | Recommended effort | Rationale |
|---|---|---|---|
| 1. Plan | `opus` | `max` | Intent clarification, codebase research, design |
| 2A. Refine | `opus` | `high` | Iterative improvement with feedback |
| 2B. Fracture | `sonnet` | `high` | Mechanical doc splitting w/ context preservation |
| 3. Singletonize | `sonnet` | `high` | Cross-check against codebase, fill gaps |
| 4. Loss Checking | `sonnet` | `medium` | Diff original vs final for dropped content |
| 5. Sequencing | `opus` | `high` | Ordered implementation plan |
| 6. Implementing | per-stage | per-stage | Each `NNN-stage-name.md` gets its own hint (mostly `sonnet`/`medium`; `opus`/`high` for architecturally sensitive stages) |
| 7. Testing | `sonnet` | `medium` | Build + test execution, failure triage |
| 8. Staging | `sonnet` | `medium` | Squash rebase + archive + PR create; needs care because force-push is destructive |
| 9. Review Gate | `haiku` | `low` | CI poll + status summary |

## Git Command Convention

**Use `git -C <repo>` instead of `cd <repo> && git ...`** for all git commands. This avoids compound shell commands (`cd && git`) which trigger unnecessary permissions prompts in Claude Code, even for trusted repositories. Determine the repo path from the current working directory or worktree root once (`git rev-parse --show-toplevel`), then pass it via `-C` for every git invocation.

**Invocation protocol**: Every time this document is invoked, the orchestrator must:
1. Detect the current workflow stage (see [State Detection](#state-detection))
2. Describe what the next action will be to the user
3. Wait for user approval before executing (unless the user has opted into auto-advance)
4. Execute the stage
5. Update `WORKFLOW_STATE.json` with the new state
6. Report results and what comes next

---

## Repo Reference: Commands and Conventions

Everything below is verified against `.github/workflows/rust.yml` and the app `package.json` files. Stage documents should reference these commands rather than inventing their own.

### Workspace layout

| Path | Cargo package | Notes |
|---|---|---|
| `core/` | `ethos-core` | Shared Rust crate |
| `core/ui/` | — | `@ethos/core` Svelte component library, consumed by the app frontends |
| `friendshipper/` + `friendshipper/src-tauri/` | `friendshipper` | Tauri app (bin target `friendshipper`) |
| `friendshipper/server/` | `friendshipper-server` | Headless server, also shipped as a container image |

### Setup (required before Rust checks on a fresh worktree)

The Tauri crate expects a frontend output directory to exist, and the app frontend consumes the packaged `core/ui` library. CI does this first, and so should you:

```bash
cd core/ui && yarn && yarn package     # build the shared component library
cd friendshipper && yarn               # app frontend deps
mkdir -p friendshipper/build
```

### Verification commands (mirror of CI)

`.github/workflows/rust.yml` is authoritative for which apps and packages CI actually builds; check its job matrices if a check here seems not to apply.

| Purpose | Command |
|---|---|
| Rust format check | `cargo fmt -p ethos-core -- --check` (repeat for `friendshipper`, `friendshipper-server`) |
| Rust format fix | `cargo fmt --all` |
| Clippy (CI-strict) | `cargo clippy --all-features -p <package> -- -D warnings` |
| Unit tests | `cargo test --release -p ethos-core`, `cargo test --release --bin friendshipper`, `cargo test --release -p friendshipper-server` |
| Frontend lint | `cd <core/ui\|friendshipper> && yarn lint` |
| Frontend lint fix | `yarn lint:fix` (apps) / `yarn format` (`core/ui`) |
| Svelte type check | `cd <app> && yarn check` (not in CI, but catches real type errors) |
| Full app build | `cd <app> && yarn tauri:build` |
| Run the app | `cd <app> && yarn tauri:dev` |

Notes:
- Clippy is `-D warnings` in CI. A warning is a failure.
- `yarn lint` runs prettier in check mode; formatting-only failures are common and are fixed with `yarn lint:fix`.
- There is no remote build-verification service for this repo. Build locally, then let PR CI (Linux + Windows) confirm.

### Git hooks (the real gate — stricter than CI)

`core.hooksPath` is set to `friendshipper/.husky`, so **every local commit** runs:

| Hook | Runs |
|---|---|
| `pre-commit` | `npx lint-staged` in `core/ui` and `friendshipper` (eslint `--fix` + prettier `--write` on staged files, re-staged automatically), then `cargo +nightly clippy --workspace --all-targets --all-features -- -D warnings`, then `cargo fmt --all --check` |
| `commit-msg` | `npx commitlint --edit --cwd friendshipper` |

Consequences:

- The pre-commit clippy is **stricter than CI**: nightly toolchain, whole workspace, `--all-targets` (so test and bench code is linted too). Code that passes the CI-equivalent commands in the table above can still be rejected locally. Requires a nightly toolchain — `rustup toolchain install nightly` if `cargo +nightly` fails.
- `cargo fmt --all --check` runs on every commit, so formatting can never drift. Run `cargo fmt --all` before committing.
- lint-staged only touches **staged** files. Unstaged frontend files are not formatted, which is why a full `yarn lint` is still worth running before staging the branch.
- **Never bypass with `--no-verify`.** If a hook fails, fix the cause. If the user explicitly asks to bypass, say what is being skipped.

### Commit conventions

Conventional commits, **enforced by commitlint** (`friendshipper/.commitlintrc.json`) — a non-conforming message is rejected by the `commit-msg` hook:

```
<type>(<scope>): <description>
```

- **type** (required, from `type-enum`): `ci`, `chore`, `docs`, `feat`, `fix`, `perf`, `refactor`, `revert`, `style`, `test`
- **scope** (required — `scope-empty` is `never`, so a bare `fix: ...` is rejected) from `scope-enum`: `core`, `friendshipper`, `misc`
  - Use `misc` for repo-wide or tooling changes. There is **no** `server` scope — changes to `friendshipper/server` go under `friendshipper` or `misc`.
- **description**: terse, lower-case, no trailing period
- No CI tags. This repo's CI runs on every PR; there is nothing to opt into.
- GitHub appends the PR number on squash-merge — do not add it by hand.

Note: some commits on `main` (bot version bumps, scope-less messages) do not satisfy these rules, because GitHub composes squash-merge subjects server-side and never runs the local hooks. Local commits still must conform.

### Branch conventions

| Kind | Pattern | Pushed? |
|---|---|---|
| Parking | `llm/<username>/<worktree>-parking-<YYYYMMDD>` | Never |
| Work (no issue) | `llm/<username>/<slug>` | Yes |
| Work (with issue) | `llm/<username>/<issue-number>-<slug>` | Yes |

`<username>` is the local part of `git config user.email`. `<slug>` is short kebab-case. Human-authored branches in this repo use `<initials>/<slug>`; the `llm/` prefix marks agent-driven work and keeps the two sets easy to tell apart.

---

## State File: WORKFLOW_STATE.json

The workflow tracks its progress via a `WORKFLOW_STATE.json` file located **in the plan directory** (`.claude/workflow/<work-slug>/`). This file is the primary source of truth for workflow state.

### Schema

```json
{
  "version": 1,
  "stage": "plan|refine|fracture|singletonize|loss_check|sequencing|implementing|testing|staging|review_gate|complete",
  "stage_detail": "Free-text description of where within the stage we are",
  "plan_document": "path to the primary plan document, relative to repo root",
  "plan_directory": ".claude/workflow/<work-slug>",
  "branch": "git branch name for this workflow",
  "worktree": "absolute path of the worktree that owns this workflow",
  "issue_url": "GitHub issue URL (if one exists)",
  "pr_url": "Pull request URL (created during staging)",
  "created": "ISO 8601 timestamp of workflow start",
  "last_updated": "ISO 8601 timestamp of last state change",
  "auto_advance": false,
  "refinement_history": [
    {
      "timestamp": "ISO 8601",
      "action": "description of what was done",
      "user_feedback": "summary of user feedback that prompted this iteration"
    }
  ],
  "fractured_documents": ["list of document paths after fracturing"],
  "sequenced_stages": ["list of NNN-stage-name.md paths after sequencing"],
  "implementation_progress": {
    "current_stage": "NNN",
    "completed_stages": ["001", "002"],
    "failed_stages": []
  }
}
```

Timestamps: read the current time from the environment context or `git log -1 --format=%cI`; do not invent one.

---

## State Detection

When invoked, determine the current stage using this priority order:

### 1. Primary: Read WORKFLOW_STATE.json

Search for `WORKFLOW_STATE.json` under `.claude/workflow/` (excluding `archive/`). If exactly one is found, read it and trust the `stage` field. Validate by spot-checking:

- Does the `branch` match the current git branch?
- Does `worktree` match the current worktree root? If not, this workflow belongs to a different worktree — warn the user and do not modify it.
- Do the referenced files still exist?
- If validation fails, warn the user and offer to re-detect via heuristics.

If more than one is found, list them and ask the user which workflow to advance.

### 2. Fallback: Heuristic Detection

If no `WORKFLOW_STATE.json` exists:

| Condition | Detected Stage |
|-----------|---------------|
| On a parking branch (`*-parking-*`) or `main` | **No workflow in progress** — start Stage 1 |
| On a work branch with no plan directory under `.claude/workflow/` | **Stage 1** — planning not yet started or just beginning |
| Plan document exists but no `*_REVIEW.md` and no fractured docs | **Stage 2A** — ready for refinement |
| Plan document exists with fractured subdocuments | **Stage 2B → 3** — check if documents are self-contained |
| Numbered stage files exist (`NNN-*.md`) | **Stage 5 complete → 6** — ready for implementation |
| Stage files exist and the branch has commits beyond `origin/main` | **Stage 6 → 7** — implementation in progress or ready for testing |
| Local verification commands pass | **Stage 7 → 8** — ready for staging |
| PR exists and branch is more than one commit ahead of main | **Stage 8** — resume staging (squash and push not yet done) |
| PR exists, branch is squashed, not yet approved | **Stage 9** — awaiting human review |
| PR exists and is approved | **Stage 9** (terminal) — merge, no further workflow steps |

When using heuristic detection, **always confirm the detected stage with the user** before proceeding.

### 3. Fresh Start

If on a parking branch or main with no workflow state:
- Inform the user that no workflow is in progress
- Ask if they want to start a new Creation Workflow (Stage 1)
- If yes, proceed to Stage 1

---

## Stage 1: Plan

**Entry condition**: No active workflow, or `stage == "plan"`
**Goal**: Clarify the user's intent and produce a grounded work proposal document

### Process

#### 1.1 Clarify Intent

Engage the user in an iterative Q&A to fully specify what they want:
- What is the feature or change?
- Which surface does it affect — the `friendshipper` app, the shared `ethos-core` crate, the shared `core/ui` component library, or `friendshipper/server`?
- Is this Rust-side, frontend-side, or both? Does it cross the Tauri command boundary?
- What is the desired user experience?
- What are the boundaries / what is explicitly out of scope?
- Are there constraints around git/LFS behavior, long-running operations, cross-platform support (Windows + Linux are both built in CI), or config/persistence?

Ask questions until the request is unambiguous. Use `AskUserQuestion` for enumerable choices, free-text for open-ended exploration. **Do not stop at the first answer** — dig into implications, edge cases, and integration points.

#### 1.2 Research

Using the ethos source:
- Identify the existing systems the feature must integrate with (Rust modules in `core/src` and `<app>/src-tauri/src`, Tauri commands, Svelte routes/stores in `<app>/src`, shared components in `core/ui/src`)
- Trace the full path for anything user-visible: Svelte component → Tauri `invoke` → command handler → core operation
- Find relevant structs, traits, and state containers (e.g. long-running operation handling, git/LFS wrappers, config types)
- Identify existing patterns the implementation should follow, and note prior art that solves a similar problem differently
- For third-party crate or Tauri/Svelte API questions, verify against the actual dependency version in `Cargo.toml` / `package.json`; use `WebFetch`/`WebSearch` against docs.rs or upstream docs rather than guessing

This research must produce **concrete references** to actual code — not guesses about what might exist. The plan should be grounded in the real codebase.

#### 1.3 Write the Work Proposal

Create the plan directory `.claude/workflow/<work-slug>/` and write the proposal there, named descriptively (e.g. `SUBMIT_QUEUE_PROPOSAL.md`). The proposal should contain:

- **Goal**: What this achieves and why it matters
- **Scope**: What's included and explicitly excluded
- **Architecture**: How this fits into the existing system, which crates/apps/modules are affected
- **Technical Design**: Key types, traits, Tauri commands, state, and UI surfaces — referencing actual existing code
- **Dependencies**: What must exist before this can be built; any new crates or npm packages (call these out explicitly, they need review)
- **Cross-platform notes**: Anything that behaves differently on Windows vs Linux
- **Risks**: What could go wrong, what's uncertain
- **Open Questions**: Anything not yet resolved

The proposal should be detailed enough that a senior engineer could evaluate feasibility, but high-level enough that it describes *what* and *why* rather than dictating *how* line-by-line. Avoid writing implementation code in the proposal.

#### 1.4 Snapshot for Loss Checking

Copy the approved proposal verbatim to `.claude/workflow/<work-slug>/00-original-plan.md`. **Never edit this file again** — Stage 4 diffs the final documents against it. Because plan docs are not committed, this snapshot is the only record of the original intent.

#### 1.5 Present for Review

Show the proposal to the user. If they approve, create the work branch.

#### 1.6 Create Work Branch

```bash
git -C <repo> fetch origin main
git -C <repo> switch -c "llm/<username>/<slug>" origin/main
```

If the user wants issue tracking, create or link a GitHub issue and use `llm/<username>/<issue>-<slug>`:

```bash
gh issue create --title "<title>" --body "<goal + scope summary>"
```

Do not push yet — the branch has no commits, and plan documents are not committed. The branch is first pushed in Stage 8. If the user wants early visibility, push an empty branch (`git -C <repo> push -u origin <branch>`) and note it in the state file.

#### 1.7 Update State

Create `WORKFLOW_STATE.json` in the plan directory with `stage: "refine"`, and record `branch`, `worktree`, `plan_document`, `plan_directory`, and `issue_url` if one exists.

**Exit condition**: Plan document written, snapshot taken, work branch created, state file created.

---

## Stage 2A: Refine

**Entry condition**: `stage == "refine"`
**Goal**: Iteratively improve the plan with user feedback

### Important Principles

- **Refinement requires fresh user input**. Running the automated review in a loop without user feedback does not improve the plan — it tends toward "implementation-as-instructions" where code ends up written inside the plan document.
- The plan should remain a **high-level design document**, not a set of copy-paste instructions.
- Each refinement cycle should incorporate the user's perspective, domain knowledge, or changed requirements.

### Process

When entering this stage, present the user with their options:

> **The plan is ready for refinement.** How would you like to proceed?

Offer these tools/actions:

1. **Run automated review** — Invoke [RefineProposal.md](RefineProposal.md) to catch internal inconsistencies, verify source code references, and generate a structured review document. Useful as a starting point for discussion, not as a final product.

2. **Generate a diagram** — Produce a mermaid chart for a specific aspect of the design (data flow across the Tauri boundary, state machine, sequence diagram, module dependencies). Embed it in the plan or present it separately.

3. **Publish a shareable summary** — If the user wants to share the design outside the terminal, render it as an Artifact.

4. **Update the GitHub issue** — If an issue exists for this work, sync the current plan state to it (goal, scope, open questions).

5. **Provide feedback directly** — The user gives free-text feedback, asks questions, or requests specific changes to the plan.

6. **Mark refinement complete** — The user is satisfied with the plan and wants to move to the next stage.

After each refinement cycle:
- Record the iteration in `WORKFLOW_STATE.json`'s `refinement_history` array
- Update the plan document with changes (never `00-original-plan.md`)
- Ask the user what they'd like to do next (loop back to options above)

### Before Marking Complete

Ensure the plan document itself is a complete record: goal, scope (including what was explicitly excluded), technical design per feature with file impacts and API shapes, architecture, numbered key decisions with rationale, risks with mitigations, and where the work originated. Anyone reading only this document should have full context without reconstructing the conversation.

If a GitHub issue exists, sync a condensed version of that summary to it — the issue is the only part of this record that survives outside the local machine.

**Exit condition**: User explicitly marks refinement complete. Update state to `stage: "fracture"`.

---

## Stage 2B: Fracture

**Entry condition**: `stage == "fracture"`
**Goal**: Break the plan into multiple documents to enable parallel editing and clear ownership of sections

### When to Fracture

Fracturing is required when:
- The plan is complex enough that editing one section could inadvertently affect another
- Multiple aspects of the plan benefit from independent, focused attention
- The eventual implementation will have clear module boundaries that map to document boundaries

If the plan is small (a single-crate change, one UI surface), say so and skip to Stage 3 with the user's agreement — record the skip in `stage_detail`.

### Process

#### 2B.1 Design the Document Structure

Analyze the plan and identify natural boundaries:
- By layer (e.g. `SUBMIT_core-rust.md`, `SUBMIT_tauri-commands.md`, `SUBMIT_ui.md`)
- By component (e.g. `<NAME>_shared-core.md`, `<NAME>_app.md`, `<NAME>_server.md`)
- By concern (e.g. `<NAME>_architecture.md`, `<NAME>_persistence.md`, `<NAME>_error-handling.md`)

Each fractured document should:
- Have a clear, non-overlapping scope
- Reference (but not duplicate) content from other documents
- Be editable independently without breaking coherence

#### 2B.2 Execute the Fracture

1. Keep the original plan document as the "overview" document in the plan directory
2. Create individual documents for each identified section
3. Add cross-references between documents
4. Update `WORKFLOW_STATE.json` with the `fractured_documents` list

`00-original-plan.md` stays untouched.

#### 2B.3 Parallel Refinement (Optional)

After fracturing, individual documents can be refined using sub-agents working in parallel. Each sub-agent:
- Reads only its assigned document plus the overview
- Makes focused improvements
- Does not modify other documents

This is optional and depends on the user's preference and the complexity of the plan.

**Exit condition**: Plan is fractured into self-contained documents. Update state to `stage: "singletonize"`.

---

## Stage 3: Singletonize

**Entry condition**: `stage == "singletonize"`
**Goal**: Harden every document so it can be implemented with zero ambiguity by a less-capable agent

### Principles

- This is **not further iteration on the design**. The design decisions are made. This stage clarifies what is written.
- Every document must contain everything needed to implement its scope without consulting other documents (though it may reference them for context).
- Assumptions and prerequisites must be made explicit.
- Vague language ("as needed", "appropriate", "similar to") must be replaced with specifics.

### Process

For each document in the plan:

#### 3.1 Verify Against Codebase

- Confirm every referenced module, type, function, Tauri command, Svelte component, and file path still exists
- Verify that described interfaces match actual signatures (Rust fn signatures, `#[tauri::command]` parameter names, serde field names, TypeScript types)
- Check that assumed behaviors match actual implementations
- Update any stale references

#### 3.2 Expose Assumptions

Scan for implicit assumptions and make them explicit:
- "This will use the existing X" → verify X exists and describe exactly how it will be used
- "The system handles Y" → specify which module, which function, what inputs/outputs
- "Follows the existing pattern" → name the pattern and cite a concrete example file in the repo
- Serialization: state the exact wire shape crossing the Tauri boundary, including serde rename/case conventions

#### 3.3 Fill Gaps

Identify and fill any missing information:
- Missing error handling — which error type, how it surfaces to the UI
- Unspecified edge cases
- Unclear ordering of operations, and anything that must not block the UI thread
- Missing type definitions on either side of the boundary
- Unaddressed persistence/config migration concerns
- Cross-platform path or process handling

#### 3.4 Simplify for Implementation

Rewrite complex descriptions so they are implementable as direct instructions:
- Replace "consider using X or Y" with a decision: "use X because [reason]"
- Replace "the system should handle [vague scenario]" with specific behavior descriptions
- Ensure every described behavior maps to a concrete implementation action

**Exit condition**: All documents are self-contained, unambiguous, and verified against the codebase. Update state to `stage: "loss_check"`.

---

## Stage 4: Loss Checking

**Entry condition**: `stage == "loss_check"`
**Goal**: Verify that nothing from the original plan or refinement feedback was dropped, ignored, or distorted

### Process

#### 4.1 Gather the Record

1. **Original plan**: `.claude/workflow/<work-slug>/00-original-plan.md` (the immutable Stage 1 snapshot)
2. **Refinement feedback**: the `refinement_history` array in `WORKFLOW_STATE.json`
3. **Current state**: the hardened, singletonized documents

#### 4.2 Compare

For each piece of the original plan and each piece of refinement feedback:
- Is it present in the final documents?
- If it was intentionally removed or changed, is the reason documented?
- If it was split across multiple documents, is the full intent preserved?

#### 4.3 Report

Produce a brief **loss report** (`LOSS_REPORT.md` in the plan directory) documenting:
- Items preserved faithfully
- Items modified (with justification)
- Items intentionally dropped (with justification)
- Items **unintentionally dropped** (these need to be restored or explicitly addressed)

If unintentional losses are found:
- Present them to the user
- Ask whether to restore them or explicitly mark them as out of scope
- Update the documents accordingly

**Exit condition**: All plan content is accounted for. Update state to `stage: "sequencing"`.

---

## Stage 5: Sequencing

**Entry condition**: `stage == "sequencing"`
**Goal**: Turn the plan into an ordered sequence of implementable stages

### Process

Invoke **[BreakDownWorkProposal.md](BreakDownWorkProposal.md)** with the plan document(s) as input.

### Important Modifications for This Workflow

The sequencing step from BreakDownWorkProposal.md should be applied with these additional considerations:

#### Test Rigor Depends on Implementation Structure

- **Sequential, stacking implementation** (each phase builds on the last): Tests at each stage must **actually verify** that the implementation compiles and functions correctly before moving on. This is test-verified implementation — not test-driven development, but each layer must be solid before the next is added.

- **Independent, parallel implementation** (phases are unrelated to each other, e.g. several standalone Tauri commands or unrelated UI panels): compile/clippy verification can be batched — a single check after several independent units is fine, since fixing an error in unit A won't affect unit B. However, **functional testing** should not be deferred if the individual units have behavioral depth (state mutation, persistence, external process invocation, error paths). Each unit that has external callers or persists state still needs its own functional test stage. What can be deferred is only the integration test across units.

The sequencing step should explicitly identify which pattern applies and annotate the stage files accordingly.

#### Entry Point

The output must include `000-prompt-to-implement-thing.md` as the single entry point. This file must:
- Reference the source proposal for full context
- List all stages in order with brief descriptions
- Include the branch name and issue URL (if any)
- Provide the commands reference table
- Explain the test rigor expectations for this specific breakdown

### Post-Sequencing

After BreakDownWorkProposal produces the stage files:
1. Review the breakdown for completeness
2. Update `WORKFLOW_STATE.json` with the `sequenced_stages` list
3. Present the breakdown to the user for review

**Exit condition**: Stage files written and reviewed. Update state to `stage: "implementing"`.

---

## Stage 6: Implementing

**Entry condition**: `stage == "implementing"`
**Goal**: Execute the sequenced stages as commits

### Process

#### 6.1 Determine Starting Point

Read `WORKFLOW_STATE.json`'s `implementation_progress` to find where to resume:
- If `current_stage` is set, resume from that stage
- If no progress recorded, start from stage `001`

#### 6.2 Execute Stages

For each stage, in order:

1. Read the stage document (`NNN-stage-name.md`)
2. **Use sub-agents** for implementation — each stage should be delegated to a sub-agent with the stage document, the source proposal, and relevant context. This preserves the orchestrator's context window for coordination.
3. **Cost optimization**: Read the stage document's front-of-file `<!-- claude-hint -->` block if present, or check the hint table in this document's "Stage Model/Effort Hints" section. Pass matching `model:`/`effort:` values to the subagent invocation via `Agent({ model: "...", ... })`. If no hint is available, default to `sonnet`/`medium`.
4. After the sub-agent completes, verify the stage's validation criteria
5. If validation passes, commit per the stage document's instructions (source/asset changes only — plan documents are not committed)
6. Update `implementation_progress` in `WORKFLOW_STATE.json`
7. If validation fails, attempt debugging (possibly with a higher-cost sub-agent), then retry

#### 6.3 Progress Tracking

After each stage:
- Update `WORKFLOW_STATE.json` with progress
- If a stage fails repeatedly (3+ attempts), mark it as failed and inform the user

#### 6.4 Make Maximum Progress

The implementing stage should push through as many stages as possible in a single invocation. Do not stop after each stage to ask the user — keep going until:
- All stages are complete
- A stage fails and cannot be resolved
- The context window is getting full (delegate more aggressively to sub-agents)
- The user intervenes

**Exit condition**: All implementation stages complete. Update state to `stage: "testing"`.

---

## Stage 7: Testing

**Entry condition**: `stage == "testing"`
**Goal**: Verify the full CI-equivalent check set passes on a rebased branch

### Process

#### 7.1 Rebase on Latest Main

```bash
git -C <repo> fetch origin main
git -C <repo> rebase origin/main
```

If conflicts arise, resolve them. This is part of the "test" — the implementation must work with the latest codebase, not just the snapshot it was developed against.

#### 7.2 Reproduce CI Locally

Run the same checks CI runs, for every package the diff touches (determine them with `git -C <repo> diff --name-only origin/main...HEAD`):

```bash
# Prerequisites (once per worktree, or after core/ui changes)
cd core/ui && yarn && yarn package
mkdir -p friendshipper/build

# Rust
cargo fmt -p <package> -- --check
cargo clippy --all-features -p <package> -- -D warnings
cargo test --release -p ethos-core
cargo test --release --bin friendshipper

# Frontend (per affected package: core/ui, friendshipper)
cd <app> && yarn && yarn lint
```

Always run the `ethos-core` checks when `core/` changed — every other package depends on it.

Also run the stricter check the `pre-commit` hook will apply, since it gates every commit in this stage and in Stage 6:

```bash
cargo fmt --all --check
cargo +nightly clippy --workspace --all-targets --all-features -- -D warnings
```

#### 7.3 Build Verification

For changes that touch Rust in a Tauri crate or the frontend build, do a real build of each affected app:

```bash
cd <app> && yarn tauri:build
```

`--no-bundle` is already baked into the `tauri:build` script, so this is the fast form of the CI build. Fix any build errors.

#### 7.4 Functional Verification

Compile-clean is not "working". For anything user-visible, run the app and exercise the change:

```bash
cd <app> && yarn tauri:dev
```

Verify the happy path and at least one error path. If the change cannot reasonably be exercised locally (needs remote infrastructure, specific repo state, or platform-specific behavior), say so explicitly and list what a human must verify — do not silently claim it works.

#### 7.5 Debug and Fix Failures

For each failure:
1. Diagnose the root cause
2. Fix the implementation
3. Re-run the failing check
4. Ensure the fix doesn't break other checks

Common CI-only failure classes worth checking proactively on the diff:
- Prettier formatting in Svelte/TS files (`yarn lint` fails where the code is otherwise fine — fix with `yarn lint:fix`)
- Clippy lints that are warnings locally but fatal in CI (`-D warnings`)
- Windows/Linux differences: path separators, path case, process spawning, line endings
- `cargo fmt` on files touched only via search-and-replace

#### 7.6 Commit Fixes

Commit any fixes as additional commits (they'll be squashed in staging).

**Exit condition**: All checks pass on the rebased branch. Update state to `stage: "staging"`.

---

## Stage 8: Staging

**Entry condition**: `stage == "staging"`
**Goal**: Archive plan documents, rebase onto latest main, run the lint/format pipeline, squash, write the final commit, push the merge-ready branch, create the PR

### Process

#### 8.1 Archive Plan Documents

Copy the design documents from the plan directory into `.claude/workflow/archive/<work-slug>/`. These documents capture the intent, reasoning, and design decisions behind the work.

What to archive:
- The plan/proposal document(s), including `00-original-plan.md`
- Fractured design documents
- Singletonized documents
- `LOSS_REPORT.md`
- Any review documents (`*_REVIEW.md`) and diagrams
- `000-prompt-to-implement-thing.md`

What NOT to archive (mechanical workflow artifacts):
- `WORKFLOW_STATE.json`
- Individual `NNN-stage-name.md` implementation instruction files (except `000-*`)

`<work-slug>` should be concise kebab-case (e.g. `submit-queue`, `lfs-lock-cache`).

Do not rewrite or clean up these documents. This is an archive, not a documentation database — copy them as-is to preserve the original context. The archive is local and gitignored; it is a record of intent, not a claim that the work merged.

#### 8.2 Verify No Workflow Artifacts Leaked Into the Diff

Plan documents are supposed to be untracked. Confirm it:

```bash
git -C <repo> diff --name-only origin/main...HEAD
```

If any path under `.claude/workflow/` appears, remove it from the branch before continuing (it was force-added past `.gitignore`). Paths under `.claude/skills/`, `.claude/instructions/`, and `.claude/scripts/` **are** tracked, so those may legitimately appear if this work changed the tooling itself. Also confirm the working tree has no stray debug scaffolding:

```bash
git -C <repo> status --porcelain
```

#### 8.3 Rebase onto Latest Main

```bash
git -C <repo> fetch origin main
git -C <repo> rebase origin/main
```

If conflicts arise, resolve them carefully — main may have moved significantly since implementation began.

Do **not** squash yet. The lint pass in 8.4 has to run against the rebased tree, and its fixes belong *inside* the squashed commit rather than amended onto it afterward.

#### 8.4 Run the Lint/Format Pipeline

This is a required gate, not a formality: the `pre-commit` hook runs `cargo +nightly clippy --workspace --all-targets --all-features -- -D warnings` and `cargo fmt --all --check` on every commit, so an unclean tree cannot even be committed in 8.5.

**1. Scope the work.**

```bash
git -C <repo> diff --name-only origin/main...HEAD
```

Map changed paths to packages. `core/` changes mean `ethos-core` plus every package that depends on it. `core/ui/` changes mean re-packaging the library and re-linting each frontend that consumes it.

**2. Satisfy the prerequisites.** Clippy on the Tauri crate needs the frontend output dir to exist, and the frontend consumes the packaged library:

```bash
cd core/ui && yarn && yarn package
mkdir -p friendshipper/build
```

**3. Apply formatting and auto-fixes.**

```bash
cargo fmt --all
cd core/ui && yarn format          # if core/ui changed
cd <app> && yarn lint:fix          # per affected app (prettier --write + eslint --fix)
```

**4. Verify clean — mirror both the hook and CI.**

```bash
cargo fmt --all --check
cargo +nightly clippy --workspace --all-targets --all-features -- -D warnings   # what pre-commit enforces
cargo clippy --all-features -p <package> -- -D warnings                         # what CI enforces, per affected package
cd <app> && yarn lint              # per affected app, and core/ui if it changed
cd <app> && yarn check             # optional; catches type errors CI does not
```

Run the nightly workspace clippy even when only one package changed — it is the check that will actually block the commit, and `--all-targets` lints test code that the CI command skips. If `cargo +nightly` is missing, install it (`rustup toolchain install nightly`) rather than working around the hook.

**5. Re-test if lint changed behavior.** Formatting is safe, but clippy fixes sometimes alter logic (iterator rewrites, borrow changes). If any fix touched more than style, re-run the affected tests:

```bash
cargo test --release -p ethos-core
cargo test --release --bin <app>
```

**6. Commit the fixes.** Land them as an ordinary commit — the squash in 8.5 folds it in:

```bash
git -C <repo> add -A
git -C <repo> commit -m "style(<scope>): apply fmt and lint fixes"
```

If nothing changed, skip the commit and continue.

**Never pass `--no-verify` anywhere in this stage.** The hook runs the same checks you just ran; if it fails, something here was skipped or a check was run against the wrong package set.

#### 8.5 Squash onto Main

**Idempotency check first**:

```bash
git -C <repo> rev-list --count origin/main..HEAD
```

- Output `1` — already squashed by a previous Stage 8 run; skip to 8.6 to verify the commit message.
- Output `0` — the branch has nothing on top of main; abort with an error.
- Output `>= 2` — squash now.

Interactive rebase is not available in this environment, so squash with `reset --soft`. The rebase in 8.3 already put HEAD on top of `origin/main`, so this is a pure history collapse:

```bash
git -C <repo> diff origin/main --stat        # record the tree state before
git -C <repo> reset --soft origin/main       # keep the tree, drop the commits
git -C <repo> commit -F <message-file>       # single squashed commit (see 8.6)
git -C <repo> diff origin/main --stat        # must match the "before" output exactly
```

If the two `diff --stat` outputs differ, stop and investigate before pushing.

This commit runs the hooks again: the tree must already be clean from 8.4, and the message must satisfy commitlint (8.6). Using `-F <message-file>` keeps the multi-line message intact and lets commitlint see exactly what will be recorded.

#### 8.6 Write the Squashed Commit Message

```
<type>(<scope>): <description>

<optional body>

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
```

Format requirements (enforced by the `commit-msg` hook — see [Commit conventions](#commit-conventions)):
- `<type>` from: `ci`, `chore`, `docs`, `feat`, `fix`, `perf`, `refactor`, `revert`, `style`, `test`
- `<scope>` is **required** and must be one of: `core`, `friendshipper`, `misc`
- Lower-case description, no trailing period
- No CI tags — this repo has none
- Do not add a PR number; GitHub adds it on squash-merge
- The `Co-Authored-By` trailer follows the agent-authorship convention. This repo's existing history does not carry these trailers; if the user prefers to match repo history, omit it — ask once and record the answer in `WORKFLOW_STATE.json`.

Example:

```
feat(friendshipper): add submit queue with retry on lock contention
```

If commitlint rejects the message, it will name the failing rule. Fix the message — do not bypass the hook.

#### 8.7 Push and Create the PR

```bash
git -C <repo> push -u origin <branch-name> --force-with-lease
```

Modern git (>= 2.30) treats a missing remote ref as "no expected value" and succeeds with a normal fast-forward push, so no fallback path is required for the first push of a new branch. Subsequent re-staging passes (e.g. after a testing → staging cycle) use `--force-with-lease` against the existing remote ref.

Then create the PR:

```bash
gh pr create --title "<conventional-commit-style title>" --body "$(cat <<'EOF'
## Summary
- <bullet points describing the changes>

## Test plan
- [ ] `cargo fmt --all --check` and `cargo +nightly clippy --workspace --all-targets --all-features -- -D warnings` clean
- [ ] `cargo test` passes for affected packages
- [ ] `yarn lint` passes for affected frontends
- [ ] `yarn tauri:build` succeeds for affected apps
- [ ] Change exercised in a running app (happy path + one error path)
- [ ] Cross-platform behavior considered (CI builds Linux + Windows)

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

Include a `Closes #<issue>` line in the body if an issue exists.

After PR creation:
- Update `WORKFLOW_STATE.json` with `pr_url`
- Report:

```
✓ Staging Complete
  Branch:  <branch-name>
  PR:      <pr-url>
  Commit:  <type>(<scope>): <description>
  Archive: .claude/workflow/archive/<work-slug>/
```

**Exit condition**: PR created. Update `WORKFLOW_STATE.json` to `stage: "review_gate"`.

---

## Stage 9: Review Gate

**Entry condition**: `stage == "review_gate"`
**Goal**: Wait for human review and approval of the PR

### Process

This is a **human gate**. The PR must be reviewed and approved by a human reviewer before it can merge.

#### 9.1 Check PR Status

```bash
gh pr view <pr-number> --json state,reviewDecision,statusCheckRollup
gh pr checks <pr-number>
```

CI runs `frontend-checks` (lint for `core/ui` and `friendshipper`), `build-linux`, and `build-windows`. Expect it to take a while; do not poll in a tight loop.

#### 9.2 Review Options

Before waiting for external review, present the user with their options:

> **The PR is ready for review.** How would you like to proceed?

1. **Run automated code review** — Invoke `/code-review` (or `/security-review` for auth/credential/network-facing changes) to analyze the changes before human review.
2. **Check PR status** — View the current approval and CI status.
3. **Monitor CI** — `gh pr checks <pr-number> --watch`.
4. **Proceed to human review** — The PR is ready; reviewers can approve it on GitHub.

#### 9.3 Status Reporting

If the PR is **not yet approved**:

```
📍 Review Gate — Awaiting human approval
   PR:     <pr-url>
   Review: <pending/changes_requested/approved>
   CI:     <passing/pending/failing>

   The PR needs human review and approval before it can merge.
```

#### 9.4 Handling Failures

If CI is **failing**:
- Diagnose from the failing job's logs (`gh run view <run-id> --log-failed`)
- Fix, then push additional commits (preserving review history) or re-run Stage 8 to re-squash if the user prefers a single commit
- Re-check CI

If review requested changes: return to Stage 6 or 7 as appropriate, then re-run Stage 8.

If the PR **has been approved** and CI is passing:

```
✓ PR approved and CI passing — ready to merge.
```

The user merges from the GitHub PR UI (squash merge). Do not merge on their behalf unless they explicitly ask.

**Exit condition**: PR approved and CI passing. Update state to `stage: "complete"`. After the merge lands, `/park` the worktree to return to a clean main.

---

## User Interaction Protocol

### Before Every Stage Transition

Unless `auto_advance` is `true` in `WORKFLOW_STATE.json`, present the user with:

```
📍 Current stage: <stage name>
   <brief description of current state>

⏭️  Next action: <stage name>
   <description of what will happen>

Proceed?
```

Options:
- **Yes, proceed** — Execute the next stage
- **Skip to stage...** — Jump to a specific stage (with warning about skipped steps)
- **Enable auto-advance** — Stop asking for approval, just proceed (sets `auto_advance: true`)
- **Describe in more detail** — Explain what the next stage involves before committing

### When the User Returns After a Break

```
📍 Resuming Creation Workflow
   Plan:    <plan document name>
   Branch:  <branch name>
   Stage:   <current stage> — <stage detail>
   Last updated: <timestamp>

What would you like to do?
```

Options:
- **Continue where we left off** — Proceed with the detected next action
- **Review current state** — Show the plan, progress, and any open issues
- **Go back to stage...** — Return to an earlier stage for revision
- **Abandon workflow** — Move the plan directory to `.claude/workflow/archive/<work-slug>-abandoned/` and `/park` the worktree

---

## Relationship to Other Instructions

| Stage | Delegates To | Notes |
|-------|-------------|-------|
| 2A (Refine) | [RefineProposal.md](RefineProposal.md) | One of several refinement tools |
| 5 (Sequencing) | [BreakDownWorkProposal.md](BreakDownWorkProposal.md) | Primary sequencing engine |
| after merge | `/park` skill | Reset the worktree to latest main |

Stages 1, 2B, 3, 4, 6, 7, 8, and 9 are defined entirely within this orchestrator.

---

## Error Handling

- **WORKFLOW_STATE.json is corrupted or inconsistent**: Warn the user, offer to re-detect state via heuristics or start fresh
- **Referenced files are missing**: Plan docs are untracked, so git history cannot recover them. Check `.claude/workflow/archive/` and ask the user before reconstructing anything
- **State file belongs to another worktree**: Do not advance it. Tell the user which worktree owns it
- **Branch has diverged from expected state**: Run `git -C <repo> log` to understand what happened. If commits exist that the workflow didn't create, ask the user about them
- **Sub-agent fails during implementation**: Record the failure, try once more with additional context, then escalate to the user
- **User wants to change the plan mid-implementation**: This is valid. Return to Stage 2A (Refine) with the understanding that completed implementation stages may need to be revisited. Update `WORKFLOW_STATE.json` accordingly

---

## Example: Full Workflow Execution

### Invocation 1: Starting Fresh

User is on `llm/<username>/ethos-parking-20260806` (a parking branch).

```
📍 No active Creation Workflow detected
   Currently on parking branch: llm/<username>/ethos-parking-20260806

Would you like to start a new Creation Workflow?
```

User: "Yes — I want Friendshipper to queue submits instead of dropping them when one is in flight."

→ Execute Stage 1 (Plan): ask clarifying questions, research `friendshipper/src-tauri/src` and `core/src`, write `.claude/workflow/submit-queue/SUBMIT_QUEUE_PROPOSAL.md`, snapshot it, create branch `llm/<username>/submit-queue`, create `WORKFLOW_STATE.json`.

### Invocation 2: Continuing After Planning

State file shows `stage: "refine"`.

```
📍 Resuming Creation Workflow
   Plan:    .claude/workflow/submit-queue/SUBMIT_QUEUE_PROPOSAL.md
   Branch:  llm/<username>/submit-queue
   Stage:   refine — Plan written, awaiting refinement

⏭️  Next action: Refine
   Present refinement options (automated review, diagram, direct feedback, etc.)

Proceed?
```

### Invocation 3: Picking Up Mid-Implementation

State file shows `stage: "implementing"`, `current_stage: "005"`.

```
📍 Resuming Creation Workflow
   Stage:     implementing — Stage 005 (implement-queue-drain) in progress
   Completed: 001, 002, 003, 004
   Remaining: 005, 006, 007, 008

⏭️  Next action: Continue implementing from Stage 005

Proceed?
```

### Invocation 4: PR Approved

```
📍 Resuming Creation Workflow
   Stage: review_gate — PR approved, CI passing
   PR:    https://github.com/<owner>/ethos/pull/570

✓ PR approved and CI passing — ready to merge.
```

→ Update state to `stage: "complete"`. The user squash-merges from the GitHub PR UI, then `/park` returns the worktree to a clean main.
