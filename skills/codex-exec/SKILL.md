---
name: codex-exec-implementation
description: Use when user explicitly asks to implement a task with Codex in non-interactive mode, such as "帮我用codex实现...".
---

# Codex Exec Implementation

## Overview
When the user explicitly asks to "use Codex to implement" a task, execute the work through Codex CLI in non-interactive mode with `codex exec`.

## When to Use
- User directly requests Codex-based implementation.
- Common trigger phrases include:
  "帮我用codex实现..."
  "use codex to implement ..."
  "用codex帮我做 ..."

## Execution Rules
1. Extract the concrete implementation task from the user request as `<task>`.
2. If `<task>` is missing or ambiguous, ask one concise clarification question first.
3. Execute with the shell tool using non-interactive Codex CLI:
`codex exec "<task>"`
4. Wait for completion and capture stdout, stderr, and exit status.
5. Return a concise result summary:
- files changed
- verification/tests run (if any)
- failures and exact next action

## Tool Call Template
Use one shell tool call and keep it non-interactive:
`{"name":"shell","arguments":{"command":"codex exec \"<task>\""}}`

## Guardrails
- Use `codex exec` only. Do not use interactive `codex` mode.
- Keep scope bounded to the user request. Do not add speculative work.
- If Codex command fails (missing binary, auth error, runtime error), return the exact command error and a concrete remediation step.

## Quick Reference
- Non-interactive command: `codex exec "<task>"`
