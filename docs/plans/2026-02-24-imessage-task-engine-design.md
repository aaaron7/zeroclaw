# iMessage Autonomous Task Engine Design (Phase 1)

## Context

ZeroClaw currently processes channel messages in an inbound-triggered, one-shot loop. In iMessage workflows this causes unfinished long tasks to stall unless the user sends a new trigger message (for example, "继续").

Observed behavior also shows completion hallucinations: the model can claim file/report completion while no verifiable filesystem write occurred.

## Scope

Phase 1 scope is intentionally constrained:

- Channel: iMessage only
- Architecture direction: shared task engine at `agent` layer (future-proof), but only wired to iMessage in phase 1
- Notifications: milestone-only
- Persistence: SQLite-backed task state with restart recovery
- Provider error policy: retry N times, then fail and notify

Out of scope for phase 1:

- Telegram/Slack/Discord integration
- Cross-channel unification of message interruption policies
- New provider fallback routing policy changes

## Goals

1. Remove dependency on user follow-up triggers for in-progress iMessage tasks.
2. Prevent "claimed completed" responses without write verification evidence.
3. Persist task progress so restart can recover unfinished work.
4. Keep blast radius low and rollback simple.

## Architecture

### Design choice

Adopt a shared `TaskEngine` in `src/agent/` and integrate only with iMessage in phase 1.

Why this shape:

- Keeps orchestration concerns out of individual channels.
- Provides a clear extension path for additional channels without copy/paste logic.
- Maintains single responsibility: channel handles transport, engine handles task lifecycle.

### Proposed modules

- `src/agent/task_engine.rs`
  - Task acceptance
  - Internal continuation loop
  - Milestone emission
  - Completion evaluation handoff
- `src/agent/task_store.rs`
  - SQLite schema management and CRUD for task runs/events/artifacts
- `src/agent/task_types.rs` (or local structs in `task_engine.rs` if minimal)
  - `TaskStatus`, `TaskRun`, `TaskEvent`, `TaskArtifact`

`src/channels/mod.rs` will be adapted so iMessage inbound messages enter `TaskEngine`, while non-iMessage channels continue using current flow.

## Data Model

SQLite file (phase 1):

- `workspace/state/task-runs.db`

Tables:

1. `task_runs`
   - `id TEXT PRIMARY KEY`
   - `channel TEXT NOT NULL`
   - `sender_key TEXT NOT NULL`
   - `reply_target TEXT NOT NULL`
   - `status TEXT NOT NULL` (`queued/running/blocked/completed/failed/cancelled`)
   - `original_request TEXT NOT NULL`
   - `last_response TEXT`
   - `attempt_count INTEGER NOT NULL DEFAULT 0`
   - `provider_retry_count INTEGER NOT NULL DEFAULT 0`
   - `created_at TEXT NOT NULL`
   - `updated_at TEXT NOT NULL`
   - `completed_at TEXT`

2. `task_events`
   - `id INTEGER PRIMARY KEY AUTOINCREMENT`
   - `task_id TEXT NOT NULL`
   - `event_type TEXT NOT NULL`
   - `payload TEXT` (JSON)
   - `created_at TEXT NOT NULL`

3. `task_artifacts`
   - `id INTEGER PRIMARY KEY AUTOINCREMENT`
   - `task_id TEXT NOT NULL`
   - `path TEXT NOT NULL`
   - `verified INTEGER NOT NULL DEFAULT 0`
   - `checksum TEXT`
   - `verified_at TEXT`

Indexes:

- `task_runs(status)`
- `task_runs(channel, sender_key, status)`
- `task_events(task_id, created_at)`
- `task_artifacts(task_id, path)`

## Execution Flow (iMessage)

1. Inbound iMessage arrives.
2. Create `TaskRun(status=queued)` and emit milestone: `accepted`.
3. Queue internal `Start(task_id)` event.
4. Engine worker transitions task to `running`, emits milestone: `started`.
5. Execute `run_tool_call_loop(...)`.
6. Evaluate output:
   - If verifiably complete -> `completed` + milestone `completed`.
   - If not complete but actionable -> enqueue `Continue(task_id)` and keep running.
   - If provider transport error -> retry up to configured limit.
   - If retries exhausted -> `failed` + milestone `failed`.

## Completion Claim-Evidence Contract

For write-intent tasks, completion requires all of:

1. A successful write-like tool execution occurred (`file_write` or write-like `shell`).
2. A post-write verification read occurred (`file_read` or read-like `shell`: `ls/cat/wc/stat`).
3. Verification output proves artifact existence and non-empty content.

If any condition is missing:

- Completion is rejected.
- Task remains in `running` and continues autonomously.
- Final user-facing "saved/completed" message is suppressed.

On successful verification:

- Store artifact evidence in `task_artifacts`.
- Emit milestone: `tool_write_verified`.

## Guardrail Upgrade Rules

Existing text-level guardrails remain as first-pass protection.

Add state-aware checks in engine completion evaluation:

- Do not trust completion text alone.
- Require evidence state for write-completion claims.
- Detect stalled loops (`K` consecutive rounds with no new tool execution or repeated progress chatter) and fail with explicit reason `stalled_loop`.

## Milestone Notification Policy

Milestones only (phase 1):

- `accepted`: task has been taken over
- `started`: execution started
- `tool_write_verified`: write + post-write verification succeeded
- `completed` or `failed`: terminal outcome

No per-iteration chatter.

## Recovery Behavior

On startup:

- Load `task_runs` where status in `queued`, `running`, `blocked`.
- Requeue `queued` and `running` for recovery execution.
- For `blocked`, emit status notice and keep blocked in phase 1 (conservative policy).

Every state transition writes `task_events` for auditability.

## Safety and Privacy

- Reuse credential scrubbing for task event payloads.
- Do not store full sensitive prompts/tool outputs when not needed; keep summaries/evidence.
- Keep default-deny behavior for tool permissions unchanged.

## Rollout Plan

Phase 1:

- iMessage integration only
- Feature gated internally by channel check (no global behavior change)
- Existing non-iMessage path untouched

Phase 2 (future):

- Integrate telegram then other channels through same engine
- Unify interruption/cancellation semantics

## Validation Strategy (Phase 1)

1. Unit tests

- task state transitions
- persistence CRUD and schema migration
- completion evaluator (positive/negative evidence cases)
- stalled loop detection

2. Integration tests

- iMessage inbound -> autonomous continuation without extra user message
- write claim without evidence is blocked
- write + verify path reaches completed
- provider retry exhaustion results in failed milestone

3. Regression checks

- existing channel flows unchanged for non-iMessage channels
- existing guardrail tests still pass

## Risks and Mitigations

1. Risk: engine/channel coupling regression
   - Mitigation: isolate engine behind narrow API and keep iMessage-only hook in phase 1.

2. Risk: false negatives in completion evaluator
   - Mitigation: conservative defaults + explicit failure reason + bounded retries.

3. Risk: restart race causing duplicate execution
   - Mitigation: sender-level serialization lock and atomic status updates.

## Rollback

- Remove iMessage routing into `TaskEngine` and revert to old per-message processing path.
- Keep task DB inert (no runtime usage) if rollback is needed quickly.
- No schema changes to critical existing stores (`cron/jobs.db`) in phase 1.
