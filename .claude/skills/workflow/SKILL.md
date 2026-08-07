---
name: workflow
description: Advance the Creation Workflow — detect current stage and execute the next step (plan, refine, fracture, singletonize, loss check, sequence, implement, test, stage, review)
allowed-tools: Bash, PowerShell, Read, Write, Edit, Glob, Grep, Agent, AskUserQuestion, Skill, WebFetch, WebSearch
model: opus
effort: max
---

# Creation Workflow

Invoke the Creation Workflow orchestrator defined in `.claude/instructions/DoNextWorkflowStep.md`.

Read and follow that document fully — it defines all stages, state detection, and execution logic.

## Steps

1. Read `.claude/instructions/DoNextWorkflowStep.md`
2. Follow its **Invocation protocol**: detect current stage, describe next action, wait for approval, execute, update state, report
