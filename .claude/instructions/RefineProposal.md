<!-- claude-hint:
model: opus
effort: high
rationale: Multi-turn Q&A review; cross-references source; deep architectural judgment
-->

Do not use instructions from this file unless asked.

# Refine Proposal

This instruction defines a review workflow for critically evaluating a work proposal document. The reviewer assumes the role of a senior engineer on a small team building cross-platform desktop developer tools in Rust (Tauri) and TypeScript (SvelteKit). Before producing the final review, the reviewer engages the author in a clarifying Q&A dialogue to build deep understanding of the proposal's intent, then produces a review document that reflects that understanding — including specific guidance on how the proposal could be updated to better communicate its goals.

## Overview

The workflow:
- Reads and deeply analyzes an input proposal document (`$DOCUMENT`)
- Evaluates it from the perspective of a senior engineer responsible for shipping and maintaining these tools
- Asks the author targeted clarifying questions to resolve ambiguities and understand intent before forming judgments
- Uses the author's answers to distinguish genuine gaps from merely unstated context
- Produces a single structured review document with categorized feedback and concrete suggestions for strengthening the proposal

## Prerequisites

- The user must provide a `$DOCUMENT` path pointing to a proposal file (typically under `.claude/workflow/<work-slug>/`)
- The repo source must be accessible for cross-referencing claims in the proposal

## Parameters

| Parameter | Required | Description |
|-----------|----------|-------------|
| `$DOCUMENT` | Yes | Path to the proposal document to review |

## Reviewer Persona

**Role**: Senior engineer, cross-platform desktop tooling
**Experience**: Deep Rust (async/tokio, error handling, process orchestration), Tauri command/IPC design, SvelteKit + TypeScript frontends, and the git/git-LFS behavior these tools automate
**Disposition**: Constructively critical. The goal is to make the proposal succeed, not to tear it down. Every question should help the author think more clearly about implementation.

**Review priorities** (in order):
1. **Correctness**: Will this actually work as described? Are the technical claims accurate?
2. **Completeness**: What's missing that would block implementation? What decisions are deferred that shouldn't be?
3. **Clarity**: Could two engineers read this and arrive at different implementations?
4. **Risk**: What are the highest-risk elements, and are they acknowledged? These tools operate on users' real repositories — data-loss risk deserves special weight.
5. **Integration**: How does this interact with existing systems? Are there hidden coupling or dependency issues, especially across the `ethos-core` / app boundary?

## Steps

### 1. Read the Proposal

1. Read the full contents of `$DOCUMENT`
2. Take note of the proposal's stated goals, scope, architecture, and any open questions it already identifies
3. Form an initial list of areas that are unclear, ambiguous, or that you need more context to evaluate

### 2. Cross-Reference Against Source Code

Investigate claims made in the proposal by searching the repo:

1. **Verify referenced systems exist**: If the proposal mentions extending or integrating with existing modules, types, Tauri commands, or Svelte components, confirm they exist and understand their current interfaces
2. **Check both sides of the boundary**: For anything user-visible, trace Svelte component → `invoke(...)` → `#[tauri::command]` handler → `ethos-core` operation. Proposals frequently describe one side and leave the other implicit
3. **Check for conflicting patterns**: Look for existing code that solves similar problems differently, which could indicate a consistency issue or missed prior art
4. **Check shared-code blast radius**: `core/` is consumed by the `friendshipper` app and `friendshipper/server`, and `core/ui/` by the app frontend. Determine whether the proposal accounts for every consumer, not just the one it names
5. **Identify touched surface area**: Determine which crates, modules, components, and config types the proposal would need to modify or depend on
6. **Note what the source code makes clear**: Some questions that seem ambiguous in the proposal may be easily answered by reading the code. Filter these out — they are not worth raising to the author

Do NOT exhaustively audit the entire codebase. Focus investigation on systems the proposal directly references or would need to interact with.

### 3. Cross-Reference Against Dependency Behavior

If the proposal makes claims about crate, Tauri, Svelte, or git/LFS behavior:

1. Verify the claim against the **version actually pinned** in `Cargo.toml` / `package.json` — this matters most for Tauri (v2 APIs differ substantially from v1) and SvelteKit
2. Use `WebFetch`/`WebSearch` against docs.rs or upstream documentation for API-level claims rather than assuming
3. For git and git-LFS behavior claims, prefer verifying against the repo's own wrapper code, which encodes hard-won behavior, over general git knowledge

Only investigate when the proposal makes specific technical claims. Do not speculatively audit dependencies.

### 4. Ask Clarifying Questions

Before forming the final review, engage the author in a dialogue to resolve ambiguities and deepen understanding. This is the core iterative step — it prevents the review from raising questions the author already has answers to.

#### 4.1. Formulate Questions

From Steps 1-3, identify questions where the author's answer would materially change the review. Good clarifying questions:
- Resolve ambiguity about intent ("Does this mean X or Y?")
- Surface unstated context the author may be carrying ("What's the expected scale/frequency of Z?")
- Probe design rationale ("Why X over Y? Is there a constraint I'm not seeing?")
- Verify understanding ("I'm reading this as [paraphrase]. Is that accurate?")

Do NOT ask questions that:
- Are already answered in the document
- Can be answered by reading the source code (you already did that in Steps 2-3)
- Are rhetorical or argumentative
- Are about minor wording choices

#### 4.2. Present Questions to the Author

Ask the author your clarifying questions. Group them by topic area for readability. Aim for the minimum number of questions needed to meaningfully improve the review — typically 3-8 questions per round.

Use `AskUserQuestion` when questions have clear enumerable options. Use conversational text when questions are open-ended.

#### 4.3. Incorporate Answers

After the author responds:
1. Update your understanding of the proposal based on their answers
2. Determine if any answers reveal new areas that need follow-up questions
3. If significant new ambiguities surfaced, ask a second (shorter) round of follow-up questions. Avoid more than 2-3 total question rounds — the goal is understanding, not interrogation
4. Note cases where the author's answer reveals that the proposal document doesn't adequately communicate the intent. These become suggestions for improving the document

### 5. Generate the Review

Produce a structured review document organized into the following sections. Each item should be a specific, actionable question or observation — not vague commentary.

#### Review Document Structure

```markdown
# Review: <Proposal Title>

**Document**: <$DOCUMENT path>
**Reviewer**: Senior engineer, desktop tooling (AI-assisted)
**Date**: <current date>

---

## Critical Issues

Items that would likely block or derail implementation if not addressed.
Each item should explain WHY it's critical and WHAT information is needed.

- **[C1]** <Title>
  <Detailed question or observation. Reference specific sections of the proposal.>

## Design Questions

Architectural or design decisions that are ambiguous, unstated, or could reasonably go multiple ways. These are questions where the answer materially affects implementation.

- **[D1]** <Title>
  <Question with context about why the answer matters.>

## Consistency Issues

Places where the proposal contradicts itself, contradicts existing code patterns, or makes incompatible claims.

- **[I1]** <Title>
  <Description of the inconsistency with references to both sides.>

## Unstated Assumptions

Things the proposal assumes to be true but does not explicitly state. Particularly dangerous when they involve the Rust/TypeScript boundary, `ethos-core` consumers other than the app in question, or the state of a user's local repository.

- **[A1]** <Title>
  <The assumption, why it might not hold, and what happens if it doesn't.>

## Cross-Platform & Repo-Safety Concerns

CI builds Linux and Windows, and these tools mutate real user repositories. Path handling, process spawning, file locking, line endings, interrupted operations, and anything that could lose uncommitted or unpushed work deserve special attention.

- **[P1]** <Title>
  <Concern with explanation of the platform or data-safety implications.>

## Scope & Risk

Observations about scope creep, underestimated complexity, or high-risk elements that deserve explicit mitigation plans. New dependencies belong here.

- **[R1]** <Title>
  <Risk description and suggested mitigation or question.>

## Minor Clarifications

Lower-priority items that would improve the proposal but are not blocking.

- **[M1]** <Title>
  <Suggestion or question.>

---

## Suggested Document Updates

Specific recommendations for how the proposal document itself could be improved to better communicate the author's intent. These are informed by the clarifying Q&A — places where the author had a good answer that simply wasn't captured in the document.

- **[U1]** <Section or area to update>
  <What to add, clarify, or restructure, and why it would help a reader.>

---

## Summary

<2-3 sentence overall assessment: what's strong about the proposal, the single most important thing to address, and a general confidence level in the proposal's implementability.>
```

#### Review Quality Guidelines

- **Be specific**: "How does X handle Y when Z?" is better than "X needs more detail"
- **Reference the proposal**: Quote or cite the specific section being questioned
- **Explain the stakes**: For each question, briefly explain what goes wrong if the answer is wrong or missing
- **Distinguish what you know from what you're asking**: If you verified something in the source, say so. If you couldn't find evidence either way, say that too
- **Don't pad**: If a section has no items, include it with "None identified." Do not fabricate concerns to fill sections
- **Prioritize within sections**: List the most important items first

### 6. Write the Review Document

1. Determine the output path:
   - Place the review file adjacent to the input document (same plan directory)
   - Name format: `<document-basename>_REVIEW.md`
   - If a file with this name already exists, confirm with the user before overwriting
2. Write the review document
3. Display a summary to the user:
   ```
   Review complete
     Document: <$DOCUMENT>
     Review:   <output-path>

     Critical issues:       <count>
     Design questions:      <count>
     Consistency issues:    <count>
     Assumptions:           <count>
     Cross-platform/safety: <count>
     Scope/Risk:            <count>
     Minor:                 <count>
     Document updates:      <count>
     Total items:           <count>
   ```

## Error Handling

- **$DOCUMENT not found**: Exit with error, ask user to verify the path
- **$DOCUMENT is empty or unreadable**: Exit with error
- **Source code not accessible**: Warn that cross-referencing is limited, proceed with proposal-only review
- **Dependency docs unreachable (offline)**: Warn that API claims cannot be verified, flag them as unverified assumptions
- **Author unresponsive to clarifying questions**: Proceed with the review based on available information, noting which items would benefit from author clarification
- **Output path not writable**: Ask user for an alternative output location

## Important Notes

- **Do not rewrite the proposal**: The output is a review document with questions and observations, not an edited version of the original. The author retains full ownership of the proposal
- **Filter out code-answerable questions**: If a question can be definitively answered by reading the source, answer it yourself rather than raising it. Only raise questions that require human judgment, domain knowledge, or design decisions
- **Respect the author's intent**: The goal is to strengthen the proposal, not redirect it. If you disagree with a fundamental design choice, frame it as a risk with alternatives, not as a directive
- **Every consumer is in scope for shared code**: Any proposal touching `core/` or `core/ui/` should be evaluated against all of its consumers (the app, its frontend, and the server), even if it names only one
- **Repo safety is always relevant**: These tools run destructive git operations on users' working copies. Evaluate every proposal for what happens on interruption, conflict, or unexpected repo state
- **Don't assume the proposal is wrong**: A question is not an accusation. Many items may have good answers the author simply didn't include. Frame feedback as "this isn't addressed" rather than "this is wrong"
