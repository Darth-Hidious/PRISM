//! Task-driven research context — the plan/artifact/handle state a long-running
//! research task carries across turns (Plan-and-Execute over `run_turn`).
//!
//! This module is **pure**: types + a deterministic block constructor. No I/O,
//! no network — everything here is unit-testable. It is the data spine of the
//! bridge architecture (TOOL_SURFACE_SPEC §5): a research campaign checkpoint
//! holds a [`ResearchTaskContext`], and `run_turn(task: Some(...))` injects the
//! [`task_context_block`] into the system prompt every iteration so the model
//! carries the goal, plan position, prior artifact handles, and working notes
//! deterministically across the inner-loop cap and across process restarts.
//!
//! ## Why this exists
//!
//! `run_turn` is chat-turn-shaped: one `user_message: &str` in, bounded by
//! `max_iterations`, no task object, no loop-level resume (audit §3). For a
//! 50-step research task that fails — the model forgets the goal, repeats
//! steps, and cannot resume. This type makes the task's durable state a
//! first-class, deterministic context block the *harness* owns and re-injects
//! every turn (research digest §4.4 — "make the trajectory deterministic").
//!
//! ## What lives here vs. the campaign engine
//!
//! The campaign engine (`crates/campaign`) owns the checkpoint spine
//! (budget/iteration caps, approval gates, resume). [`ResearchTaskContext`] is
//! the *content* of a research campaign's checkpoint — the goal, the plan, the
//! artifact references, the notes. It serializes (serde) into the checkpoint
//! JSON and deserializes on resume.

use serde::{Deserialize, Serialize};

/// A reference to a result spilled to durable memory rather than inlined in
/// context. The model sees the `summary` + `bytes` and pulls the full content
/// via `recall(query=…)` / `fetch_artifact(id=…)` on demand (SPEC §5.2 —
/// references, not blobs). Formalizes the existing `process_large_result`
/// pattern (`agent_loop.rs`) into a cross-step, task-scoped contract.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactHandle {
    /// Provenance record id (e.g. `"prov:01H…"`).
    pub id: String,
    /// One-line distinguishing summary (≤ ~200 chars) — enough for the model
    /// to decide whether to pull the full content.
    pub summary: String,
    /// Approximate byte size of the full content (the model can gauge whether
    /// a `recall` is worth it).
    #[serde(default)]
    pub bytes: usize,
}

/// One step in a research plan. Plans are Plan-and-Execute shaped (research
/// digest §2.2): a planner emits the steps; the executor (`run_turn`) advances
/// [`ResearchTaskContext::plan_position`] one step per turn. Steps are
/// deliberately coarse and natural-language — the model reasons within a step,
/// the harness tracks *which* step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanStep {
    /// Short human-readable description of this step's goal.
    pub description: String,
    /// Whether this step has been completed. The harness marks a step done when
    /// a turn driving it returns `TurnComplete` without exhausting
    /// `max_iterations` on a recoverable error.
    #[serde(default)]
    pub completed: bool,
}

/// Durable, task-scoped state injected into every task-driven turn. Owned by
/// the campaign checkpoint; passed to `run_turn` as `task: Option<&Self>`.
///
/// When `None`, `run_turn` behaves exactly as the chat path does today
/// (ReAct, no task block) — chat is never broken.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResearchTaskContext {
    /// The research goal in the user's words. The north star, injected every
    /// turn so the model never drifts from it.
    pub goal: String,
    /// Hard constraints / success criteria (e.g. "cite ≥3 primary sources",
    /// "density < 12 g/cm³"). Empty when there are none.
    #[serde(default)]
    pub constraints: Vec<String>,
    /// The ordered plan. Empty plans are valid (pure ReAct-per-turn under a
    /// standing goal); non-empty plans drive Plan-and-Execute advancement.
    #[serde(default)]
    pub plan: Vec<PlanStep>,
    /// Index into `plan` of the step the next turn should drive. 0-based;
    /// equal to `plan.len()` once all steps are complete.
    #[serde(default)]
    pub plan_position: usize,
    /// Artifact handles produced by prior steps, referenced (not inlined).
    /// This is how node-fetched data and large results survive compaction and
    /// feed later steps (SPEC §5.2, §5.3).
    #[serde(default)]
    pub artifacts: Vec<ArtifactHandle>,
    /// Model-facing working notes — the task's running hypotheses, dead-ends,
    /// and next-step intent. This is the missing "write" op for long-task
    /// memory (research digest §4.1, §4.4). The harness appends to it; the
    /// model sees it every turn.
    #[serde(default)]
    pub notes: Vec<String>,
}

impl ResearchTaskContext {
    /// Create a new task context for a goal with no precomputed plan (pure
    /// ReAct-per-turn under a standing goal — the model plans within turns).
    #[must_use]
    pub fn new(goal: impl Into<String>) -> Self {
        Self {
            goal: goal.into(),
            constraints: Vec::new(),
            plan: Vec::new(),
            plan_position: 0,
            artifacts: Vec::new(),
            notes: Vec::new(),
        }
    }

    /// The step the next turn should drive, if any. `None` when the plan is
    /// empty or fully complete.
    #[must_use]
    pub fn current_step(&self) -> Option<&PlanStep> {
        self.plan.get(self.plan_position).filter(|s| !s.completed)
    }

    /// Mark the current step complete and advance the position. No-op if there
    /// is no current step. Idempotent if called twice on the last step.
    pub fn advance(&mut self) {
        if let Some(step) = self.plan.get_mut(self.plan_position) {
            step.completed = true;
            self.plan_position += 1;
            // Skip over any already-completed steps (e.g. after a resume that
            // re-loaded a plan with completed tail steps).
            while let Some(s) = self.plan.get(self.plan_position) {
                if s.completed {
                    self.plan_position += 1;
                } else {
                    break;
                }
            }
        } else {
            // Past the end — clamp.
            self.plan_position = self.plan.len();
        }
    }

    /// Whether all plan steps are complete (the task is done, by the plan).
    /// An empty plan is NOT complete (there is nothing to complete — the task
    /// is open-ended ReAct-under-a-goal).
    #[must_use]
    pub fn is_complete(&self) -> bool {
        !self.plan.is_empty() && self.plan.iter().all(|s| s.completed)
    }

    /// Record an artifact handle produced this task.
    pub fn record_artifact(&mut self, handle: ArtifactHandle) {
        self.artifacts.push(handle);
    }

    /// Append a working note.
    pub fn add_note(&mut self, note: impl Into<String>) {
        self.notes.push(note.into());
    }
}

/// Cap the artifact summaries shown in the block (keep the block bounded;
/// full content is one `recall` away).
const ARTIFACTS_SHOWN: usize = 8;
/// Cap the working notes shown (most-recent first).
const NOTES_SHOWN: usize = 6;

/// Build the deterministic TASK CONTEXT system block (SPEC §5.1).
///
/// Returns `None` when the task has no signal worth injecting (e.g. an empty
/// goal). The block is pure — given the same context it always produces the
/// same string, so the model sees a stable, replayable view of the task every
/// iteration. Injected alongside (not replacing) the TRAJECTORY / SESSION
/// MEMORY blocks in `run_turn`.
///
/// Chat turns (`task: None`) never call this, so chat output is byte-for-byte
/// unchanged — enforced by `run_turn`'s chat-path-unchanged test.
#[must_use]
pub fn task_context_block(task: &ResearchTaskContext) -> Option<String> {
    let goal = task.goal.trim();
    if goal.is_empty() && task.plan.is_empty() {
        return None;
    }

    let mut out = String::from(
        "TASK CONTEXT — long-running research task. Every tool call should \
         serve this goal. Do not lose sight of it across steps.\n",
    );

    out.push_str(&format!("  goal: {goal}\n"));

    if !task.constraints.is_empty() {
        out.push_str("  constraints:\n");
        for c in &task.constraints {
            out.push_str(&format!("    - {c}\n"));
        }
    }

    // Plan position — the Plan-and-Execute pointer.
    if task.plan.is_empty() {
        out.push_str("  plan: (none precomputed — plan within turns toward the goal)\n");
    } else {
        let done = task.plan.iter().filter(|s| s.completed).count();
        out.push_str(&format!(
            "  plan position: step {} of {} ({} done)\n",
            task.plan_position + 1,
            task.plan.len(),
            done,
        ));
        if let Some(step) = task.current_step() {
            out.push_str(&format!("  this turn's step: {}\n", step.description));
        } else if task.is_complete() {
            out.push_str("  this turn's step: (plan complete — verify and report)\n");
        }
    }

    // Prior artifacts — references, NOT inlined (SPEC §5.2).
    if !task.artifacts.is_empty() {
        let shown = task.artifacts.len().min(ARTIFACTS_SHOWN);
        let start = task.artifacts.len().saturating_sub(shown);
        out.push_str(&format!(
            "  prior artifacts (reference, not inlined — pull with recall(query=…) or \
             fetch_artifact(id=…)): {} total, showing most recent {}\n",
            task.artifacts.len(),
            shown,
        ));
        for handle in task.artifacts.iter().skip(start) {
            out.push_str(&format!(
                "    [{}] {} (~{} bytes)\n",
                handle.id, handle.summary, handle.bytes,
            ));
        }
    }

    // Working notes — most-recent first, bounded.
    if !task.notes.is_empty() {
        let start = task.notes.len().saturating_sub(NOTES_SHOWN);
        out.push_str("  working notes (running hypotheses / dead-ends / intent):\n");
        for note in task.notes.iter().skip(start) {
            out.push_str(&format!("    - {note}\n"));
        }
    }

    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(desc: &str) -> PlanStep {
        PlanStep {
            description: desc.into(),
            completed: false,
        }
    }

    #[test]
    fn empty_goal_and_plan_produces_no_block() {
        // Nothing to inject — chat-equivalent. Prevents a noisy empty block.
        let task = ResearchTaskContext::new("");
        assert!(task_context_block(&task).is_none());
    }

    #[test]
    fn block_contains_goal_constraints_and_step() {
        let mut task = ResearchTaskContext::new("Find creep-resistant alloys for 1200C turbines");
        task.constraints = vec!["density < 12 g/cm^3".into(), "cite >=3 sources".into()];
        task.plan = vec![step("survey refractory HEAs"), step("rank by creep")];
        task.plan_position = 0;

        let block = task_context_block(&task).expect("non-empty task yields a block");
        assert!(block.contains("TASK CONTEXT"));
        assert!(block.contains("Find creep-resistant alloys"));
        assert!(block.contains("density < 12 g/cm^3"));
        assert!(block.contains("step 1 of 2"));
        assert!(block.contains("survey refractory HEAs"));
    }

    #[test]
    fn advance_moves_plan_position() {
        let mut task = ResearchTaskContext::new("g");
        task.plan = vec![step("a"), step("b"), step("c")];
        assert_eq!(task.plan_position, 0);
        assert_eq!(
            task.current_step().map(|s| s.description.as_str()),
            Some("a")
        );
        task.advance(); // completes "a"
        assert_eq!(task.plan_position, 1);
        assert_eq!(
            task.current_step().map(|s| s.description.as_str()),
            Some("b")
        );
        assert!(!task.is_complete());
    }

    #[test]
    fn advance_skips_already_completed_steps_on_resume() {
        // A resumed plan where step 0 was already marked completed before this
        // process loaded it: advance() must not get stuck.
        let mut task = ResearchTaskContext::new("g");
        task.plan = vec![
            PlanStep {
                description: "done1".into(),
                completed: true,
            },
            PlanStep {
                description: "done2".into(),
                completed: true,
            },
            step("live"),
        ];
        task.plan_position = 0;
        task.advance(); // completes done1, then skips done2 → lands on "live"
        assert_eq!(
            task.current_step().map(|s| s.description.as_str()),
            Some("live")
        );
        assert!(!task.is_complete());
    }

    #[test]
    fn is_complete_only_when_all_steps_done() {
        let mut task = ResearchTaskContext::new("g");
        assert!(!task.is_complete()); // empty plan
        task.plan = vec![step("a"), step("b")];
        assert!(!task.is_complete());
        task.advance();
        assert!(!task.is_complete());
        task.advance();
        assert!(task.is_complete());
    }

    #[test]
    fn block_lists_artifacts_as_references_not_inlined() {
        let mut task = ResearchTaskContext::new("g");
        task.record_artifact(ArtifactHandle {
            id: "prov:abc".into(),
            summary: "federated query: 42 Ti-alloys".into(),
            bytes: 51_200,
        });
        let block = task_context_block(&task).expect("block present");
        assert!(block.contains("[prov:abc]"));
        assert!(block.contains("federated query: 42 Ti-alloys"));
        assert!(block.contains("~51200 bytes"));
        assert!(block.contains("recall(query="));
        assert!(block.contains("not inlined"));
    }

    #[test]
    fn block_lists_working_notes_most_recent() {
        let mut task = ResearchTaskContext::new("g");
        for i in 0..(NOTES_SHOWN + 3) {
            task.add_note(format!("note {i}"));
        }
        let block = task_context_block(&task).expect("block present");
        // The most recent NOTES_SHOWN notes appear; older ones are dropped.
        assert!(block.contains(&format!("note {}", NOTES_SHOWN + 2)));
        assert!(!block.contains("note 0"));
        assert!(block.contains("working notes"));
    }

    #[test]
    fn serde_roundtrip_preserves_task_state() {
        // The checkpoint serialization must survive a save/load cycle (resume).
        let mut task = ResearchTaskContext::new("roundtrip goal");
        task.constraints = vec!["c1".into()];
        task.plan = vec![step("p1"), step("p2")];
        task.advance();
        task.record_artifact(ArtifactHandle {
            id: "prov:1".into(),
            summary: "s".into(),
            bytes: 10,
        });
        task.add_note("a note");

        let json = serde_json::to_string(&task).expect("serialize");
        let back: ResearchTaskContext = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(task, back);
        assert_eq!(back.plan_position, 1);
    }

    #[test]
    fn block_shows_plan_complete_message_when_done() {
        let mut task = ResearchTaskContext::new("g");
        task.plan = vec![step("only")];
        task.advance();
        let block = task_context_block(&task).expect("block present");
        assert!(block.contains("plan complete"));
    }

    #[test]
    fn no_plan_shows_plan_within_turns_message() {
        let task = ResearchTaskContext::new("a standing goal with no precomputed plan");
        let block = task_context_block(&task).expect("block present");
        assert!(block.contains("plan within turns"));
    }

    #[test]
    fn chat_path_none_task_produces_no_block() {
        // This is the chat-path-unchanged contract expressed at the decision
        // point run_turn uses: `task.and_then(task_context_block)`. A None
        // task (chat) yields None → no system block is pushed → the messages
        // vector is byte-for-byte the prior chat behavior.
        let chat_task: Option<&ResearchTaskContext> = None;
        assert!(chat_task.and_then(task_context_block).is_none());
    }
}
