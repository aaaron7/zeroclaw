# iMessage Autonomous Task Engine Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Implement a persistent iMessage-only task engine that autonomously continues unfinished tasks, enforces write-completion evidence, and recovers on restart.

**Architecture:** Add a shared `agent`-layer `TaskEngine` with SQLite-backed task state and evidence tracking. Wire only iMessage inbound traffic to the engine in phase 1, while leaving non-iMessage channels on the existing message-processing path. Keep completion decisions state-aware (evidence contract), not text-only.

**Tech Stack:** Rust, tokio async runtime, rusqlite, existing channel runtime and `run_tool_call_loop`, cargo test.

---

### Task 1: Add Task Persistence Layer (`task_store`)

**Files:**
- Create: `src/agent/task_store.rs`
- Modify: `src/agent/mod.rs`
- Test: `src/agent/task_store.rs` (module-local tests)

**Step 1: Write the failing test (schema init + roundtrip)**

Add tests in `src/agent/task_store.rs` for:
- DB schema initialization creates `task_runs`, `task_events`, `task_artifacts`
- Insert/run-state update/read-back roundtrip

Example skeleton:

```rust
#[test]
fn task_store_initializes_schema_and_roundtrips_task_run() {
    let tmp = tempfile::TempDir::new().unwrap();
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();

    let store = TaskStore::new(&workspace).unwrap();
    let task_id = "task-1";
    store.insert_task_run(/* ... */).unwrap();
    store.update_status(task_id, TaskStatus::Running).unwrap();

    let row = store.get_task_run(task_id).unwrap().unwrap();
    assert_eq!(row.status, TaskStatus::Running);
}
```

**Step 2: Run test to verify it fails**

Run:

```bash
cargo test task_store_initializes_schema_and_roundtrips_task_run -- --nocapture
```

Expected: FAIL (module/type missing)

**Step 3: Write minimal implementation**

Implement in `src/agent/task_store.rs`:
- `TaskStore::new(workspace_dir: &Path)`
- Internal `with_connection` (mirroring `cron::store` style)
- schema init SQL for 3 tables + indexes
- minimal CRUD:
  - `insert_task_run`
  - `update_status`
  - `set_last_response`
  - `get_task_run`
  - `list_recoverable_tasks`
  - `append_event`
  - `upsert_artifact_verification`

Expose module via `src/agent/mod.rs`.

**Step 4: Run test to verify it passes**

Run:

```bash
cargo test task_store_initializes_schema_and_roundtrips_task_run -- --nocapture
```

Expected: PASS

**Step 5: Commit**

```bash
git add src/agent/task_store.rs src/agent/mod.rs
git commit -m "feat(agent): add sqlite task store for autonomous task runs"
```

---

### Task 2: Add Task Types and Transition Rules

**Files:**
- Create: `src/agent/task_types.rs`
- Modify: `src/agent/mod.rs`
- Test: `src/agent/task_types.rs`

**Step 1: Write the failing test (transition legality)**

Add tests for allowed/blocked transitions:
- `queued -> running` allowed
- `running -> completed` allowed
- `completed -> running` rejected

```rust
#[test]
fn task_status_rejects_invalid_backward_transition() {
    assert!(TaskStatus::can_transition(TaskStatus::Queued, TaskStatus::Running));
    assert!(!TaskStatus::can_transition(TaskStatus::Completed, TaskStatus::Running));
}
```

**Step 2: Run test to verify it fails**

```bash
cargo test task_status_rejects_invalid_backward_transition -- --nocapture
```

Expected: FAIL

**Step 3: Write minimal implementation**

Define:
- `TaskStatus` enum (`Queued`, `Running`, `Blocked`, `Completed`, `Failed`, `Cancelled`)
- `TaskRunRecord`, `TaskEventRecord`, `TaskArtifactRecord`
- `TaskStatus::as_str`, parse helpers
- `TaskStatus::can_transition`

**Step 4: Run test to verify it passes**

```bash
cargo test task_status_rejects_invalid_backward_transition -- --nocapture
```

Expected: PASS

**Step 5: Commit**

```bash
git add src/agent/task_types.rs src/agent/mod.rs
git commit -m "feat(agent): add task run domain types and status transitions"
```

---

### Task 3: Add Completion Evaluator (Claim-Evidence Contract)

**Files:**
- Create: `src/agent/task_completion.rs`
- Modify: `src/agent/mod.rs`
- Test: `src/agent/task_completion.rs`

**Step 1: Write failing tests**

Cover:
- write-claim text + no evidence => `NotComplete`
- write evidence + post-write read evidence => `Complete`
- non-write informational response => `Complete` (if no claim)

```rust
#[test]
fn completion_evaluator_blocks_write_claim_without_evidence() {
    let result = evaluate_completion(/* response: "已保存到...", evidence: none */);
    assert_eq!(result, CompletionDecision::NotComplete);
}
```

**Step 2: Run failing tests**

```bash
cargo test completion_evaluator_blocks_write_claim_without_evidence -- --nocapture
```

Expected: FAIL

**Step 3: Minimal implementation**

Implement:
- `CompletionDecision` enum (`Complete`, `NotComplete`, `Failed(String)` optional)
- evaluator inputs:
  - final text
  - saw_write_success
  - saw_post_write_read
  - verified_artifacts
- claim detection reuse/align with existing `looks_like_filesystem_write_claim` behavior semantics

**Step 4: Run tests**

```bash
cargo test completion_evaluator -- --nocapture
```

Expected: PASS

**Step 5: Commit**

```bash
git add src/agent/task_completion.rs src/agent/mod.rs
git commit -m "feat(agent): enforce write completion claim-evidence contract"
```

---

### Task 4: Implement TaskEngine Core Loop (iMessage-phase)

**Files:**
- Create: `src/agent/task_engine.rs`
- Modify: `src/agent/mod.rs`
- Test: `src/agent/task_engine.rs`

**Step 1: Write failing tests (engine flow)**

Add tests for:
- accept -> queued -> running transition
- provider transport failure retries up to N then failed
- stalled loop detection after K no-progress rounds

```rust
#[tokio::test]
async fn task_engine_retries_provider_transport_error_then_fails() {
    // scripted provider always transport-fails
    // assert retry count and terminal failed status
}
```

**Step 2: Run tests to fail**

```bash
cargo test task_engine_retries_provider_transport_error_then_fails -- --nocapture
```

Expected: FAIL

**Step 3: Minimal implementation**

Implement:
- `TaskEngine::new(...)`
- `accept_imessage_task(...) -> task_id`
- `run_once(task_id)` and internal continuation scheduling
- provider retry policy A (configurable max retries from existing reliability config or local constant for phase 1)
- milestone callbacks/events:
  - accepted
  - started
  - write_verified
  - completed/failed

Use existing `run_tool_call_loop` and map its output through `CompletionEvaluator`.

**Step 4: Run tests**

```bash
cargo test task_engine_ -- --nocapture
```

Expected: PASS

**Step 5: Commit**

```bash
git add src/agent/task_engine.rs src/agent/mod.rs
git commit -m "feat(agent): add autonomous task engine with retry and continuation loop"
```

---

### Task 5: Wire iMessage Path to TaskEngine (Phase 1 Integration)

**Files:**
- Modify: `src/channels/mod.rs`
- Modify: `src/channels/imessage.rs` (only if needed for sender key normalization helpers)
- Test: `src/channels/mod.rs`

**Step 1: Write failing integration-style test**

Add channel test proving iMessage request can trigger autonomous continuation without requiring second inbound message.

```rust
#[tokio::test]
async fn imessage_task_engine_continues_without_followup_message() {
    // setup runtime context with iMessage message
    // scripted provider returns progress-like text then valid completion after tool evidence
    // assert final message emitted without second inbound trigger
}
```

**Step 2: Run test to fail**

```bash
cargo test imessage_task_engine_continues_without_followup_message -- --nocapture
```

Expected: FAIL

**Step 3: Minimal integration implementation**

In `process_channel_message` path:
- if `msg.channel == "imessage"`, route to `TaskEngine` acceptance path
- non-iMessage remains existing behavior unchanged

Ensure sender-level serialization for iMessage tasks only.

**Step 4: Run targeted tests**

```bash
cargo test process_channel_message_ -- --nocapture
cargo test imessage_task_engine_continues_without_followup_message -- --nocapture
```

Expected: PASS

**Step 5: Commit**

```bash
git add src/channels/mod.rs src/channels/imessage.rs
git commit -m "feat(channels): route imessage tasks through autonomous task engine"
```

---

### Task 6: Restart Recovery Hook

**Files:**
- Modify: `src/channels/mod.rs`
- Modify: `src/agent/task_engine.rs`
- Test: `src/agent/task_engine.rs` and/or `src/channels/mod.rs`

**Step 1: Write failing test**

```rust
#[tokio::test]
async fn task_engine_recovers_running_tasks_on_startup() {
    // seed store with running task
    // initialize engine
    // assert recovery event queued and task resumes
}
```

**Step 2: Run failing test**

```bash
cargo test task_engine_recovers_running_tasks_on_startup -- --nocapture
```

Expected: FAIL

**Step 3: Minimal implementation**

- On runtime/channel startup for iMessage-enabled runtime, call `TaskEngine::recover_pending()`.
- Requeue `queued/running` tasks; keep `blocked` unchanged with explicit event.

**Step 4: Run tests**

```bash
cargo test task_engine_recovers_running_tasks_on_startup -- --nocapture
```

Expected: PASS

**Step 5: Commit**

```bash
git add src/agent/task_engine.rs src/channels/mod.rs
git commit -m "feat(agent): recover pending imessage task runs on startup"
```

---

### Task 7: Milestone Notification Delivery

**Files:**
- Modify: `src/agent/task_engine.rs`
- Modify: `src/channels/mod.rs`
- Test: `src/agent/task_engine.rs` or `src/channels/mod.rs`

**Step 1: Write failing test**

```rust
#[tokio::test]
async fn task_engine_emits_only_milestone_notifications() {
    // assert accepted/started/write_verified/completed only
    // no per-iteration chatter
}
```

**Step 2: Run failing test**

```bash
cargo test task_engine_emits_only_milestone_notifications -- --nocapture
```

Expected: FAIL

**Step 3: Minimal implementation**

- Create milestone formatter for iMessage channel.
- Emit only configured milestones.
- Ensure failed includes reason summary.

**Step 4: Run tests**

```bash
cargo test task_engine_emits_only_milestone_notifications -- --nocapture
```

Expected: PASS

**Step 5: Commit**

```bash
git add src/agent/task_engine.rs src/channels/mod.rs
git commit -m "feat(agent): add milestone-only notifications for imessage task runs"
```

---

### Task 8: Documentation Update (Runtime Contract)

**Files:**
- Modify: `docs/channels-reference.md`
- Modify: `docs/troubleshooting.md`
- Modify: `docs/operations-runbook.md`
- (Optional if wording changed significantly): localized equivalents per repo i18n contract

**Step 1: Write doc assertions checklist (failing-by-absence)**

Checklist items:
- iMessage autonomous continuation behavior documented
- write-completion evidence contract documented
- recovery/retry behavior documented
- known phase-1 limits (iMessage-only) documented

**Step 2: Run quick doc consistency checks**

Run:

```bash
rg -n "iMessage|autonomous|task run|write verification|recovery" docs/channels-reference.md docs/troubleshooting.md docs/operations-runbook.md
```

Expected: before edit missing/incomplete sections

**Step 3: Update docs minimally**

Add concise sections:
- feature behavior
- failure modes
- rollback knobs (if any)

**Step 4: Re-run doc checks**

```bash
rg -n "iMessage|autonomous|task run|write verification|recovery" docs/channels-reference.md docs/troubleshooting.md docs/operations-runbook.md
```

Expected: all sections present

**Step 5: Commit**

```bash
git add docs/channels-reference.md docs/troubleshooting.md docs/operations-runbook.md
git commit -m "docs(channels): document imessage autonomous task engine behavior"
```

---

### Task 9: Full Validation and Final Integration Commit

**Files:**
- Modify as needed for final fixes only

**Step 1: Run formatting check**

```bash
cargo fmt --all -- --check
```

Expected: PASS

**Step 2: Run lint**

```bash
cargo clippy --all-targets -- -D warnings
```

Expected: PASS

**Step 3: Run tests**

```bash
cargo test
```

Expected: PASS

**Step 4: If failures, fix minimally and re-run**

Apply minimal diffs only for failing cases; avoid unrelated refactors.

**Step 5: Final commit**

```bash
git add -A
git commit -m "feat(imessage): add persistent autonomous task engine with verified completion"
```

---

## Notes for Implementer

- Preserve existing non-iMessage behavior exactly in phase 1.
- Do not introduce new heavy dependencies.
- Keep task engine API narrow and channel-agnostic for phase 2 reuse.
- Keep retries bounded and deterministic.
- Ensure all failure paths produce explicit task events and user-visible milestone failure message.

## Rollback Strategy

- Revert iMessage routing to legacy direct `process_channel_message` tool loop path.
- Keep new task DB files unused (harmless) if rollback is immediate.
- Revert module exports for `task_engine/task_store/task_completion/task_types` if full rollback required.
