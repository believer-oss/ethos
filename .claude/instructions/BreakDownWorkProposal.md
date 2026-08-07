<!-- claude-hint:
model: opus
effort: high
rationale: Senior-engineer persona; produces stage documents from proposal
-->

Do not use instructions from this file unless asked.

# Break Down Work Proposal

This instruction takes a work proposal document and breaks it down into sequentially implementable stages, each with its own document, following a skeleton-test-(implement-test)+ pattern. The output is a set of numbered stage files and a master prompt that an implementer can follow start to finish.

## Overview

The instruction should:
- Review an input proposal document and understand the full scope of work
- Break the work into discrete, ordered stages following the skeleton-test pattern
- Author individual `NNN-stage-name.md` files alongside the proposal document
- Author `000-prompt-to-implement-thing.md` as the master implementation guide

Stage files live in the plan directory (`.claude/workflow/<work-slug>/`) and are **not committed** — `.gitignore` excludes `.claude/workflow/`.

## Prerequisites

The user must provide:
- **`$INPUT_PROPOSAL_FILE`**: Path to the proposal document to break down

The invoker will derive:
- **`$PROPOSAL_DIR`**: Directory containing `$INPUT_PROPOSAL_FILE` (stage files are placed here)
- **`$PROPOSAL_NAME`**: Filename stem of the proposal, used for naming context

The work branch already exists — it is created in Stage 1 of the Creation Workflow. Do not create one here.

## Role

Assume the role of a **senior engineer** on a small team building cross-platform desktop developer tools: Rust + Tauri v2 backends, SvelteKit + TypeScript frontends, shared `ethos-core` crate and `core/ui` component library. Approach the breakdown with:
- Emphasis on incremental correctness — every stage must leave the workspace compiling and lint-clean
- Awareness that each stage will be implemented by an LLM agent that can write code, run commands, and commit work
- Awareness that CI is strict: `cargo clippy -- -D warnings` and prettier-checked frontends

## Steps

### 1. Review the Proposal

1. Read `$INPUT_PROPOSAL_FILE` thoroughly
2. Identify:
   - All new **Rust types** to be introduced (structs, enums, traits, error variants) and where they live (`core/` vs `<app>/src-tauri/`)
   - All new **Tauri commands** and their registration site
   - All new or modified **frontend surfaces** (routes, components, stores, TypeScript types)
   - **Dependencies** between proposed types and existing modules
   - **Config/persistence** changes, including migration of existing user config
   - **Risk areas** — places where incorrect implementation would cascade into future stages, and anything that mutates a user's repository
3. Build a mental model of the dependency graph: what must exist before what. Rust types generally come before commands, which come before UI

### 2. Design the Stage Breakdown

Break the work into stages following this pattern:

```
001-skeleton.md          — Type framework: Rust types, command stubs, TS types, registration
002-test-skeleton.md     — Verify the workspace builds and lints clean
003-implement-<area>.md  — Fill out first functional area of the skeleton
004-test-<area>.md       — Test the first functional area
005-implement-<area>.md  — Fill out next functional area
006-test-<area>.md       — Test the next functional area
...
NNN-final-validation.md  — End-to-end validation of the complete implementation
000-prompt-to-implement-thing.md — Master guide for the implementer
```

#### Stage Design Principles

1. **Stage 001 is always the skeleton**: Create all new types, function signatures, `#[tauri::command]` stubs (registered in the app's invoke handler), TypeScript type definitions, and any `Cargo.toml`/`package.json` dependency additions. No implementation logic — just the framework that subsequent stages fill in. This establishes the full type graph so later stages can reference any type without surprises.

2. **Every odd stage (after 001) implements; every even stage tests**: The test stage validates the preceding implementation stage. Test stages end with a commit.

3. **Each implementation stage fills in the skeleton** rather than iteratively expanding it. Stage 003 should not add new types — it should flesh out types created in 001.

4. **Each stage must leave the workspace compiling and clippy-clean**: No stage should introduce errors that the next stage is expected to fix. Stubs must return sensible defaults, and unused-code warnings must be handled deliberately (implement the caller in the same stage, or annotate with a `// TODO: Stage NNN` comment plus whatever clippy needs to stay quiet — remember CI treats warnings as errors).

5. **Test type and thoroughness scale with risk**:
   - **Rust changes** → `cargo fmt -p <pkg> -- --check`, `cargo clippy --all-features -p <pkg> -- -D warnings`, `cargo test -p <pkg>` / `--bin <app>`
   - **Frontend changes** → `yarn lint` in the affected app; `yarn check` for type errors
   - **Changes to `core/` or `core/ui/`** → verify every consumer (the `friendshipper` app and `friendshipper/server`)
   - **Full app build** → `cd <app> && yarn tauri:build`
   - **Behavior a user can observe** → `cd <app> && yarn tauri:dev` and exercise it, including at least one error path
   - **Comment-only or doc-only changes** → if the next stage recompiles anyway, skip the redundant build; do a quick grep/diff check for formatting correctness instead

6. **Each test stage ends with a commit** following the repo's convention, which commitlint enforces via the `commit-msg` hook:
   ```
   <type>(<scope>): <description>
   ```
   Types: `ci`, `chore`, `docs`, `feat`, `fix`, `perf`, `refactor`, `revert`, `style`, `test`. Scope is **required** and must be one of `core`, `friendshipper`, `misc` (there is no `server` scope). No CI tags — this repo has none. No PR number — GitHub adds it on squash-merge.

7. **Every commit runs the pre-commit hook**, which is stricter than CI: lint-staged (prettier + eslint `--fix`) on staged frontend files, then `cargo +nightly clippy --workspace --all-targets --all-features -- -D warnings`, then `cargo fmt --all --check`. Stage documents must therefore treat `cargo fmt --all` and a clean nightly workspace clippy as part of every stage's validation, not just the final one. Never instruct the implementer to pass `--no-verify`.

8. **The pattern is flexible**: skeleton-test-(implement-test)+ is the default, but stages can be reordered, combined, or restructured to fit the work. Multiple small implementation stages might share a single test stage; a risky stage might get two (unit + functional).

### 3. Author Stage Documents

Each stage document follows this format:

```markdown
# Stage NNN: <Stage Title>

## Goal
One-sentence description of what this stage accomplishes.

## Prerequisites
- List stages that must be completed first (e.g. "Stage 001 completed and committed")
- Any setup required (e.g. "core/ui packaged: cd core/ui && yarn && yarn package")

## Inputs
- Files, types, or data this stage reads or depends on

## Steps

### Step 1: <Action>
Detailed instructions the implementer follows.
Include exact file paths, type names, function signatures, and code patterns where possible.

### Step 2: <Action>
...

## Outputs
- Files created or modified
- Expected state after completion

## Validation
How to verify this stage succeeded before moving on. Reference the concrete commands:

- **Format**: `cargo fmt --all` then `cargo fmt --all --check`
- **Lint (Rust, CI)**: `cargo clippy --all-features -p <pkg> -- -D warnings`
- **Lint (Rust, pre-commit hook — stricter, gates the commit)**: `cargo +nightly clippy --workspace --all-targets --all-features -- -D warnings`
- **Tests**: `cargo test --release -p ethos-core` / `cargo test --release --bin <app>`
- **Lint (frontend)**: `cd <app> && yarn lint`
- **Type check**: `cd <app> && yarn check`
- **Build**: `cd <app> && yarn tauri:build`
- **Run**: `cd <app> && yarn tauri:dev`
- **Diff check**: grep/diff review for non-compiling stages

## Commit
Commit message for this stage (only for test stages or stages that produce committed work):
```
<type>(<scope>): <description>
```

## Notes
Any caveats, known issues, or things the implementer should watch for.
```

#### Skeleton Stage (001) Specifics

The skeleton stage must:
- Add all new Rust modules, structs, enums, and error variants with `todo!()`-free compiling stubs (return `Default`, empty collections, or a clear `Err(...)` — never `todo!()`/`unimplemented!()`, which turn a stub into a runtime panic)
- Declare new modules in their parent `mod.rs` / `lib.rs` / `main.rs`
- Add `#[tauri::command]` stubs and register them in the app's `invoke_handler`
- Add matching TypeScript types/interfaces on the frontend
- Add any new dependencies to the workspace `Cargo.toml` and the package's `Cargo.toml`, or the app's `package.json`
- Add `// TODO: Implement in Stage NNN` comments marking where each subsequent stage will work
- Leave `cargo clippy -- -D warnings` clean — dead-code warnings on not-yet-called stubs are the most common way this stage fails

#### Test Stage Specifics

Test stages fall into two categories. The stage author must choose the appropriate category based on what was implemented.

##### Verification Test Stages (post-skeleton, post-minor-change)

For stages where the implementation is stubs, scaffolding, or low-risk changes:
- Specify the exact commands to run, including package flags
- Define clear pass/fail criteria
- End with a commit instruction

##### Functional Test Stages (post-meaningful-implementation)

For stages that implemented behavior a user or external caller will interact with — Tauri commands, git/LFS operations, background workers, UI flows — the test stage must **actually run the code and verify its outputs**. A compile check alone provides false confidence.

Functional test stages must include:

1. **Discovery step**: identify real inputs to test against (an actual repo, actual config, actual files). Do not invent synthetic fixtures when real state is available.

2. **Happy-path exercise**: run the app (`yarn tauri:dev`) or the relevant unit test and invoke each implemented operation with real inputs. Verify the output contains expected data and is correctly shaped. For multi-step flows, exercise the full lifecycle: start → operate → finish.

3. **Error-path exercise**: for each operation, provide at least one invalid input (missing file, dirty repo, no network, cancelled operation). Verify a clean error surfaces to the UI — not a panic, not a silent failure, not a frozen window.

4. **Round-trip verification** (for anything that persists state): mutate → persist → restart the app → verify the mutation survived. Config and cached state that don't round-trip are broken regardless of what other tests say.

5. **Cross-validation** (recommended): confirm the result through an independent path — inspect the file on disk, or run the equivalent `git` command by hand — so a bug where the tool believes it wrote something is caught.

6. **Fix-iterate loop** (explicit step): after running all tests, catalog every failure. For each: diagnose root cause → fix → re-run → repeat. **Do not commit until all checks pass.** Iteration is expected and normal at this stage.

7. **Commit only when clean**: the commit at the end of a functional test stage certifies that the implementation works, not just that it compiles.

##### Choosing the Test Type

| What was implemented | Test type |
|---------------------|-----------|
| Skeleton / stubs / scaffolding | Verification |
| Dependency, config, or formatting changes | Verification |
| Tauri command with a frontend caller | Functional |
| Git/LFS operation or external process invocation | Functional |
| State that persists to disk or config | Functional |
| Internal refactor with no behavior change | Verification |

When in doubt, use functional. A test stage that over-tests is better than one that under-tests.

### 4. Author the Master Prompt (000)

Create `000-prompt-to-implement-thing.md` in the plan directory:

```markdown
# Implementation Guide: <Proposal Name>

## Overview
Brief description of what is being implemented and why.

## Source Proposal
`$INPUT_PROPOSAL_FILE` — Read this first for full context.

## Branch
- **Branch**: `llm/<username>/<slug>`
- **Issue**: `<url or "none">`

## How to Follow the Stages

1. Read this document and the source proposal first
2. Execute stages in numerical order (001, 002, 003, ...)
3. Each stage is a self-contained document: `NNN-stage-name.md`
4. Follow every step in the stage document
5. Run the validation specified at the end of each stage
6. Commit when the stage document says to commit
7. Do not skip ahead — later stages depend on earlier ones

## Stage Summary

| Stage | Name | Type | Description |
|-------|------|------|-------------|
| 001   | ...  | Skeleton | ... |
| 002   | ...  | Test | ... |
| ...   | ...  | ...  | ... |

## Commands Reference

Setup (once per worktree, and after any `core/ui` change):

| Command | Purpose |
|---------|---------|
| `cd core/ui && yarn && yarn package` | Build the shared Svelte component library |
| `cd <app> && yarn` | Install app frontend dependencies |
| `mkdir -p <app>/build` | Tauri crates expect the frontend output dir to exist |

Verification:

| Command | Purpose |
|---------|---------|
| `cargo fmt --all` | Apply formatting |
| `cargo fmt --all --check` | Format check (what the pre-commit hook runs) |
| `cargo clippy --all-features -p <pkg> -- -D warnings` | Lint, CI-equivalent |
| `cargo +nightly clippy --workspace --all-targets --all-features -- -D warnings` | Lint, what the pre-commit hook runs — stricter than CI |
| `cargo test --release -p ethos-core` | Shared crate tests |
| `cargo test --release --bin <app>` | App tests |
| `cd <app> && yarn lint` | Prettier + ESLint check |
| `cd <app> && yarn lint:fix` | Apply frontend fixes (`yarn format` for core/ui) |
| `cd <app> && yarn check` | svelte-check type validation |
| `cd <app> && yarn tauri:build` | Build the app (no bundle) |
| `cd <app> && yarn tauri:dev` | Run the app |

Packages: `ethos-core`, `friendshipper`, `friendshipper-server`. Frontends: `friendshipper`, plus the shared `core/ui` library.

## Tracking Work

- Each test stage commit marks a checkpoint
- If a stage fails validation, fix it before moving on — do not accumulate debt
- Plan and stage documents are local-only (`.claude/workflow/` is gitignored); never commit them

## Commit Convention

```
<type>(<scope>): <description>
```

Enforced by commitlint via the `commit-msg` hook:

- **type**: one of `ci`, `chore`, `docs`, `feat`, `fix`, `perf`, `refactor`, `revert`, `style`, `test`
- **scope**: **required**, one of `core`, `friendshipper`, `misc` (use `misc` for repo-wide/tooling; there is no `server` scope)
- **description**: terse, lower-case, no trailing period
- No CI tags, no PR numbers
- Never commit with `--no-verify`
```

### 5. Present for Review

1. Summarize the breakdown for the user: stage count, the implement/test rhythm, and which stages carry the most risk
2. Do not begin implementation until the breakdown is approved
3. Stage files are untracked, so there is nothing to commit or push at this step

## Error Handling

- **Proposal is too vague**: Ask the user for clarification on scope, types, and boundaries before breaking down
- **Proposal scope is enormous**: Suggest splitting into multiple proposals, each with its own breakdown
- **Cannot determine type graph**: Document assumptions in stage 001 and flag for review
- **Circular dependencies in stages**: Restructure stages to break the cycle; consider combining stages if necessary

## Important Notes

- **Do not implement anything during breakdown**: This instruction only produces documents
- **Stage files are living documents**: The implementer or reviewer may revise stages after initial authoring. The breakdown is a plan, not a contract
- **Each stage must be independently understandable**: An implementer reading stage 005 should be able to understand what to do without re-reading 001-004 (though they should reference prerequisites)
- **Prefer more stages over fewer**: Smaller stages are easier to validate, review, and roll back. A stage that takes more than ~30 minutes to implement is probably too large
- **Reference the standard commands, never re-implement them**: use the commands table above rather than inlining ad-hoc build or test logic
- **The 000 file is the entry point**: An implementer starting from scratch reads 000 first, then proceeds through the stages. It must be self-sufficient as a starting guide
- **Test stages for user-facing work are not build wrappers**: A test stage that only runs `cargo clippy` is appropriate after a skeleton stage. For stages implementing Tauri commands, git operations, or UI flows, the test stage must run the code with real inputs, verify real outputs, test error paths, and verify round-trip correctness for anything persisted

## Example Stage Breakdown

For a proposal that adds a submit queue to Friendshipper so concurrent submits are serialized instead of dropped:

```
000-prompt-to-implement-thing.md   — Master guide
001-skeleton.md                    — Queue types in core, command stubs, TS types, registration
002-test-skeleton.md               — fmt + clippy + build clean; commands callable and return stub errors
003-implement-queue-state.md       — Queue storage, ordering, and state transitions in ethos-core
004-test-queue-state.md            — Functional: unit tests over enqueue/drain/cancel, including contention
005-implement-tauri-commands.md    — Wire commands to the queue, surface progress events
006-test-tauri-commands.md         — Functional: invoke from a running app, verify events and error paths
007-implement-ui.md                — Queue panel, progress display, cancel affordance
008-test-ui.md                     — Functional: exercise in tauri:dev, verify happy path + failed submit
009-final-validation.md            — Both apps build, full lint/test sweep, restart round-trip, code-reading checklist
```
