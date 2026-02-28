# Contract Completion Engine Design (Phase 2)

## Context

ZeroClaw has already introduced autonomous multi-round execution through `TaskEngine` for iMessage and web dashboard paths. This improved one-shot limitations, but completion decisions still rely on text-pattern heuristics in critical paths.

Observed production behavior shows recurring false completion:

- Promise-style progress replies are treated as terminal.
- Hallucinated "analysis complete" answers can pass without real file-read evidence.
- New user wording frequently creates new bad cases, requiring patch-by-patch phrase updates.

This phase upgrades completion control from phrase matching to contract-driven, evidence-based verification.

## Scope

In scope:

- Channels: `iMessage` and `Web Dashboard` only.
- Replace completion gate with `TaskContract + EvidenceLedger + StateMachine`.
- Enable gray-zone verifier by default.
- Verifier failure policy: conservative continue (never complete on verifier failure).
- Remove keyword-based completion logic from the terminal decision path.

Out of scope:

- Telegram/Discord/Slack channel migration.
- Provider routing redesign.
- Large persistent schema changes beyond minimal compatibility extensions.

## Goals

1. Completion must require verifiable evidence, not wording.
2. Bad-case handling must generalize to unseen phrasing.
3. Blocked outcomes must be explicit, evidence-backed, and actionable.
4. Runtime behavior remains reversible with low rollback blast radius.

## Non-Goals

1. Perfect natural-language understanding for all requests.
2. Multi-agent planning graph (DAG) orchestration.
3. Full cross-channel policy unification in this phase.

## Key Decisions (Confirmed)

1. Delivery approach: `TaskContract + StateMachine + Gray-zone Verifier`.
2. Channel rollout: `iMessage + Web Dashboard`.
3. Gray-zone verifier: enabled by default.
4. Verifier failure behavior: conservative continue.
5. Migration strength: aggressive removal of keyword-based completion in the terminal gate.

## Architecture

### Core Components

1. `TaskContractCompiler`
- Input: original request, channel context, available tools, autonomy policy.
- Output: `TaskContract` with explicit evidence requirements and terminal criteria.

2. `EvidenceLedger`
- Collects per-round tool execution evidence.
- Normalizes tool outcomes into auditable evidence records.

3. `ContractGate`
- Deterministic contract evaluator.
- Only outputs: `complete`, `continue`, `blocked`, `failed`.

4. `GrayZoneVerifier`
- Invoked only when deterministic gate returns a gray state.
- Produces structured decision with missing evidence and next action.
- Failure path returns `continue`.

5. `TaskStateMachine`
- States: `accepted -> running -> verifying -> completed | blocked | failed | stalled`.
- Terminal transitions require contract-consistent evidence.

### Integration Points

- `src/agent/task_engine.rs`
  - Orchestrates state transitions and verifier invocation.
- `src/agent/task_completion.rs` (refactor target)
  - Becomes contract/evidence gate adapter rather than phrase matcher.
- `src/channels/mod.rs` and `src/gateway/ws.rs`
  - Continue routing into shared `TaskEngine` with no channel-specific completion logic.

## Contract Model

### TaskContract

- `task_type`: `search | write_artifact | workspace_analysis | mixed | unknown`
- `required_evidence`: list of hard requirements.
- `acceptable_terminal_modes`: `completed | blocked`.
- `blocked_criteria`: evidence-backed blocked conditions with required remediation text.
- `max_rounds`, `max_stall_rounds`.
- `verification_policy`: `strict` (phase default).

### Evidence Requirements (Examples)

1. Search task:
- At least one successful search-capable tool call.
- Optional source-count requirement for "hot/trending/news" requests.

2. Write task:
- Successful write evidence.
- Post-write read/check evidence for same artifact target.

3. Workspace analysis task:
- Successful read-like evidence (`file_read`, `ls/find/rg`, etc.).

### EvidenceRecord

- `round`
- `tool_name`
- `success`
- `resource` (path/url/query)
- `metadata` (status/source_count/bytes)
- `timestamp`

## State Machine Behavior

1. `running`
- Executes one round.
- Appends evidence delta.

2. `verifying`
- Deterministic `ContractGate` check first.
- If satisfied -> `completed`.
- If blocked criteria satisfied -> `blocked` (with remediation).
- If missing evidence -> `running` with explicit next-action guidance.
- If hard failure -> `failed`.

3. Gray-zone verifier
- Triggered only on near-terminal ambiguity.
- If verifier says done and deterministic gate can accept -> `completed`.
- If verifier fails/timeouts -> `continue`.

4. Stall and budget
- `max_rounds` exhausted -> `failed`.
- Consecutive rounds with no new evidence -> `stalled`.

## Blocked Outcome Rules

Blocked is valid only when all conditions hold:

1. Real tool-level failure evidence exists.
2. Failure maps to an allowed blocked criterion.
3. Response includes concrete remediation (for example `allowed_roots` update).

If not all conditions hold, state remains `continue`.

## Migration Strategy

### Aggressive Completion-Gate Migration

- Remove keyword/phrase matching from terminal completion decision path.
- Keep optional text hints only for UX-level progress messaging, not for final state transitions.
- Contract/evidence gate becomes the single source of truth for completion.

### Compatibility

- Maintain existing task store and events.
- Add contract/evidence snapshots via event payload extensions first.
- Avoid disruptive schema expansion in phase 2 initial rollout.

## Validation Plan

1. Unit tests
- Contract compilation for key task types.
- Evidence normalization for tool outputs.
- Contract gate outcomes: complete/continue/blocked/failed.
- Verifier fallback behavior.

2. Integration tests
- iMessage autonomous flow with no user follow-up.
- Web dashboard parity flow.
- Search/write/workspace-analysis evidence gating.

3. Replay regression suite
- Seed known production bad cases as fixture conversations.
- Require pass for merge.

## Observability

Track counters and rates:

- `completion_without_required_evidence` (target near zero)
- `blocked_without_tool_error_evidence` (target zero)
- `stalled_rate`
- `verifier_invocation_rate`
- `verifier_failure_continue_rate`

## Rollout

1. Implement behind a dedicated completion-engine switch.
2. Run in shadow mode for iMessage + web dashboard.
3. Compare legacy vs contract decision diffs.
4. Promote contract engine to primary.

## Risks and Mitigations

1. Over-strict contracts reduce completion rate
- Mitigation: gray-zone verifier + explicit remediation guidance.

2. Verifier instability
- Mitigation: conservative continue policy and bounded verifier timeout.

3. Contract compiler misses intent classes
- Mitigation: unknown-task default is continue, not complete.

## Rollback

1. Disable contract completion engine switch.
2. Revert to previous completion evaluator path.
3. Keep evidence/event logs for postmortem and replay expansion.

