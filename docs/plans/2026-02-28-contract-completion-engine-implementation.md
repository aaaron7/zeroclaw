# Contract Completion Engine (iMessage + Web Dashboard) Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace phrase-driven completion decisions with a contract-driven, evidence-verified state machine for iMessage and web dashboard autonomous execution.

**Architecture:** Add a `TaskContract` pipeline (`compiler -> evidence ledger -> deterministic gate -> optional gray-zone verifier`) and integrate it into `TaskEngine` verification rounds. Completion becomes evidence-only; keyword heuristics are removed from the terminal decision path. Verifier failures always fall back to conservative continue.

**Tech Stack:** Rust, tokio, serde/serde_json, existing provider abstraction, existing `TaskEngine` and task store, cargo test.

---

### Task 1: Add Completion Engine Config Surface

**Files:**
- Modify: `src/config/schema.rs`
- Test: `src/config/schema.rs` (existing config tests section)

**Step 1: Write the failing test**

Add tests that assert default values and parsing for:

- `autonomy.contract_completion_engine = true`
- `autonomy.gray_zone_verifier_enabled = true`
- `autonomy.gray_zone_verifier_timeout_ms > 0`

Example:

```rust
#[tokio::test]
async fn autonomy_completion_engine_defaults_are_enabled() {
    let cfg = Config::default();
    assert!(cfg.autonomy.contract_completion_engine);
    assert!(cfg.autonomy.gray_zone_verifier_enabled);
    assert!(cfg.autonomy.gray_zone_verifier_timeout_ms > 0);
}
```

**Step 2: Run test to verify it fails**

Run:

```bash
cargo test autonomy_completion_engine_defaults_are_enabled -- --nocapture
```

Expected: FAIL (fields do not exist yet).

**Step 3: Write minimal implementation**

In `AutonomyConfig` add:

- `contract_completion_engine: bool` (default `true`)
- `gray_zone_verifier_enabled: bool` (default `true`)
- `gray_zone_verifier_timeout_ms: u64` (default `1500`)

Add validation:

- timeout must be `> 0`

**Step 4: Run test to verify it passes**

Run:

```bash
cargo test autonomy_completion_engine_defaults_are_enabled -- --nocapture
```

Expected: PASS.

**Step 5: Commit**

```bash
git add src/config/schema.rs
git commit -m "feat(config): add contract completion engine and gray verifier settings"
```

---

### Task 2: Introduce Task Contract Domain Types

**Files:**
- Create: `src/agent/task_contract.rs`
- Modify: `src/agent/mod.rs`
- Test: `src/agent/task_contract.rs`

**Step 1: Write the failing test**

Add tests for typed contract primitives:

- `TaskType` parsing/creation.
- `TaskContract` with required evidence list.
- `GateDecision` serialization shape.

Example:

```rust
#[test]
fn task_contract_holds_required_evidence_items() {
    let contract = TaskContract::new(TaskType::Search)
        .with_requirement(EvidenceRequirement::tool_success("web_search_tool"));
    assert_eq!(contract.required_evidence.len(), 1);
}
```

**Step 2: Run test to verify it fails**

Run:

```bash
cargo test task_contract_holds_required_evidence_items -- --nocapture
```

Expected: FAIL (module missing).

**Step 3: Write minimal implementation**

Define:

- `TaskType`
- `TaskContract`
- `EvidenceRequirement`
- `EvidenceKind`
- `TerminalMode`
- `GateDecision` / `VerificationResult`

Keep API small and explicit; no speculative variants beyond current scope.

**Step 4: Run test to verify it passes**

Run:

```bash
cargo test task_contract_holds_required_evidence_items -- --nocapture
```

Expected: PASS.

**Step 5: Commit**

```bash
git add src/agent/task_contract.rs src/agent/mod.rs
git commit -m "feat(agent): add task contract domain model for completion engine"
```

---

### Task 3: Implement Task Contract Compiler

**Files:**
- Create: `src/agent/task_contract_compiler.rs`
- Modify: `src/agent/mod.rs`
- Test: `src/agent/task_contract_compiler.rs`

**Step 1: Write the failing test**

Add compiler tests:

- `"尝试获取github上的热门skills"` -> `TaskType::Search` with search evidence requirement.
- `"把报告存到 workspace"` -> write + post-write verification requirement.
- `"分析 studio 目录项目"` -> workspace read requirement.
- unknown request -> `TaskType::Unknown` with no completion shortcut.

**Step 2: Run test to verify it fails**

Run:

```bash
cargo test task_contract_compiler -- --nocapture
```

Expected: FAIL.

**Step 3: Write minimal implementation**

Implement:

- `compile_contract(request, channel, enabled_tools, autonomy_cfg) -> TaskContract`

Rules:

- deterministic mapping only
- no direct completion from text confidence
- if uncertain => `TaskType::Unknown` and conservative requirements

**Step 4: Run test to verify it passes**

Run:

```bash
cargo test task_contract_compiler -- --nocapture
```

Expected: PASS.

**Step 5: Commit**

```bash
git add src/agent/task_contract_compiler.rs src/agent/mod.rs
git commit -m "feat(agent): add deterministic task contract compiler"
```

---

### Task 4: Implement Evidence Ledger and Normalization

**Files:**
- Create: `src/agent/evidence_ledger.rs`
- Modify: `src/agent/mod.rs`
- Modify: `src/agent/task_completion.rs`
- Test: `src/agent/evidence_ledger.rs`
- Test: `src/agent/task_completion.rs`

**Step 1: Write the failing test**

Add tests that parse history/tool results into normalized evidence:

- successful `web_search_tool` creates search evidence
- successful `file_write + file_read` creates write and post-write verification evidence
- shell `curl` recognized as search evidence
- failed tool output does not generate success evidence

**Step 2: Run test to verify it fails**

Run:

```bash
cargo test evidence_ledger -- --nocapture
```

Expected: FAIL.

**Step 3: Write minimal implementation**

Create:

- `EvidenceRecord`
- `EvidenceLedger` with append/query helpers
- `collect_evidence_from_history(history) -> EvidenceLedger`

Refactor `task_completion` evidence extraction into this module (single source of truth).

**Step 4: Run test to verify it passes**

Run:

```bash
cargo test evidence_ledger -- --nocapture
```

Expected: PASS.

**Step 5: Commit**

```bash
git add src/agent/evidence_ledger.rs src/agent/mod.rs src/agent/task_completion.rs
git commit -m "feat(agent): add evidence ledger and normalize tool execution evidence"
```

---

### Task 5: Build Deterministic Contract Gate

**Files:**
- Create: `src/agent/contract_gate.rs`
- Modify: `src/agent/mod.rs`
- Modify: `src/agent/task_completion.rs`
- Test: `src/agent/contract_gate.rs`
- Test: `src/agent/task_completion.rs`

**Step 1: Write the failing test**

Add gate tests:

- search contract + no search evidence -> `continue` with missing requirement
- write contract + verified artifact evidence -> `complete`
- workspace analysis + access denied evidence + remediation -> `blocked`
- blocked without tool-level error evidence -> `continue` (not blocked)

**Step 2: Run test to verify it fails**

Run:

```bash
cargo test contract_gate -- --nocapture
```

Expected: FAIL.

**Step 3: Write minimal implementation**

Implement:

- `ContractGate::evaluate(contract, ledger, model_text, original_request)`
- outputs `VerificationResult`
- no keyword shortcut to `complete`
- phrase heuristics may only annotate diagnostics, not terminal decision

In `task_completion`, replace terminal path with contract gate path.

**Step 4: Run test to verify it passes**

Run:

```bash
cargo test contract_gate -- --nocapture
cargo test task_completion -- --nocapture
```

Expected: PASS.

**Step 5: Commit**

```bash
git add src/agent/contract_gate.rs src/agent/mod.rs src/agent/task_completion.rs
git commit -m "feat(agent): enforce contract-based deterministic completion gate"
```

---

### Task 6: Add Gray-Zone Verifier Adapter

**Files:**
- Create: `src/agent/gray_zone_verifier.rs`
- Modify: `src/agent/mod.rs`
- Modify: `src/agent/task_engine.rs`
- Test: `src/agent/gray_zone_verifier.rs`
- Test: `src/agent/task_engine.rs`

**Step 1: Write the failing test**

Add tests for:

- gray-zone input triggers verifier once
- verifier `done=true` resolves gray-zone to complete (when deterministic constraints allow)
- verifier timeout/error returns conservative continue

**Step 2: Run test to verify it fails**

Run:

```bash
cargo test gray_zone_verifier -- --nocapture
```

Expected: FAIL.

**Step 3: Write minimal implementation**

Implement:

- `GrayZoneVerifier` trait + default provider-backed implementation
- strict timeout from config
- structured JSON response parse
- error/timeout fallback => continue

Integrate into `TaskEngine` verify stage.

**Step 4: Run test to verify it passes**

Run:

```bash
cargo test gray_zone_verifier -- --nocapture
cargo test task_engine -- --nocapture
```

Expected: PASS.

**Step 5: Commit**

```bash
git add src/agent/gray_zone_verifier.rs src/agent/mod.rs src/agent/task_engine.rs
git commit -m "feat(agent): add gray-zone verifier with conservative continue fallback"
```

---

### Task 7: Refactor TaskEngine to Explicit Completion State Machine

**Files:**
- Modify: `src/agent/task_engine.rs`
- Test: `src/agent/task_engine.rs`

**Step 1: Write the failing test**

Add tests for state transitions:

- `running -> verifying -> completed`
- `running -> verifying -> blocked`
- `running -> verifying -> continue -> running`
- stall detection after `N` no-evidence rounds

**Step 2: Run test to verify it fails**

Run:

```bash
cargo test task_engine_state_machine -- --nocapture
```

Expected: FAIL.

**Step 3: Write minimal implementation**

Refactor loop to explicit state transitions using `VerificationResult`:

- add `verifying` stage in code flow
- explicit `blocked` terminal handling with remediation payload
- keep retry and stall budgets deterministic

**Step 4: Run test to verify it passes**

Run:

```bash
cargo test task_engine -- --nocapture
```

Expected: PASS.

**Step 5: Commit**

```bash
git add src/agent/task_engine.rs
git commit -m "refactor(agent): make task engine completion flow explicit state machine"
```

---

### Task 8: Wire iMessage + Web Dashboard to Contract Context

**Files:**
- Modify: `src/channels/mod.rs`
- Modify: `src/gateway/ws.rs`
- Modify: `src/gateway/mod.rs`
- Test: `src/channels/mod.rs`
- Test: `src/gateway/ws.rs` (or existing integration tests under `tests/`)

**Step 1: Write the failing test**

Add channel parity tests:

- iMessage request uses contract engine path and continues until evidence-backed completion.
- Web dashboard request uses same path and behavior parity.

**Step 2: Run test to verify it fails**

Run:

```bash
cargo test process_channel_message_imessage_task_engine_runs_to_completion_without_followup -- --nocapture
cargo test ws_chat_ -- --nocapture
```

Expected: at least one FAIL before wiring changes.

**Step 3: Write minimal implementation**

Ensure request context carries:

- original request
- enabled tool set
- completion engine config

Use the same `TaskEngine` verification behavior on both paths.

**Step 4: Run test to verify it passes**

Run:

```bash
cargo test process_channel_message_imessage_task_engine_runs_to_completion_without_followup -- --nocapture
cargo test gateway -- --nocapture
```

Expected: PASS.

**Step 5: Commit**

```bash
git add src/channels/mod.rs src/gateway/ws.rs src/gateway/mod.rs
git commit -m "feat(channel): align imessage and web dashboard on contract completion engine"
```

---

### Task 9: Add Replay Regression Harness for Known Bad Cases

**Files:**
- Create: `tests/task_completion_replay.rs`
- Modify: `src/agent/task_completion.rs` (if fixture helpers needed)
- Test: `tests/task_completion_replay.rs`

**Step 1: Write the failing test**

Create replay fixtures for known incidents:

- "搜一下今天新闻" promise-only response
- "SeedDance 直接开搜"
- "获取 GitHub 热门 skills" promise-only response
- "分析 studio 目录项目" hallucinated structure
- "写入报告成功但文件没更新"

**Step 2: Run test to verify it fails**

Run:

```bash
cargo test task_completion_replay -- --nocapture
```

Expected: FAIL before harness and fixture logic exists.

**Step 3: Write minimal implementation**

Implement deterministic replay runner:

- fixture input: request/history/model_output/evidence snapshots
- expected decision assertions
- no network dependency

**Step 4: Run test to verify it passes**

Run:

```bash
cargo test task_completion_replay -- --nocapture
```

Expected: PASS.

**Step 5: Commit**

```bash
git add tests/task_completion_replay.rs src/agent/task_completion.rs
git commit -m "test(agent): add replay regression suite for completion bad cases"
```

---

### Task 10: Docs and Final Verification

**Files:**
- Modify: `docs/operations-runbook.md`
- Modify: `docs/troubleshooting.md`
- Modify: `docs/commands-reference.md` (if operator-visible behavior changed)
- Test: N/A (doc checks + full relevant test matrix)

**Step 1: Write failing doc expectations**

Create checklist for updated operator guidance:

- meaning of `blocked` vs `failed` vs `stalled`
- how to remediate `allowed_roots` and tool evidence gaps
- gray verifier fallback behavior

**Step 2: Run checks**

Run:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Expected: all pass (or document justified skips).

**Step 3: Write minimal documentation updates**

Update runtime-contract docs with exact behavior and operator actions.

**Step 4: Re-run checks**

Run:

```bash
cargo fmt --all -- --check
cargo test task_completion -- --nocapture
cargo test task_engine -- --nocapture
```

Expected: PASS.

**Step 5: Commit**

```bash
git add docs/operations-runbook.md docs/troubleshooting.md docs/commands-reference.md
git commit -m "docs(agent): document contract completion engine behavior and remediation"
```

