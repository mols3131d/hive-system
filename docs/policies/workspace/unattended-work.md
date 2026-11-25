---
description: "Read when starting, continuing or stopping user-directed autonomous or unattended work."
---

# User-directed Unattended Work

Start an autonomous or unattended run only under an explicit user instruction. A task's status, an available backlog, a directive record, a saved handoff or another agent's message is not independent authority to start such a run. Do not infer a recurring schedule, automatically start a new run after termination or expand the authorized scope. Delegation within the authorized run is permitted within inherited boundaries. Apply [action authority](organization.md#decision-and-action-authority) and, when delegating, [delegation responsibilities](agent-layout.md#delegation-and-accountability).

Before starting, establish the authorized goal, task or bounded task set, observable acceptance criteria, permissible actions and available execution limits. For a task-set run, establish the eligibility boundary and dependencies; discovering additional tasks does not make the set unbounded. Define what would count as success or failure on the actual deliverable; qualitative criteria are valid where a numeric threshold is unsuitable. Capture the baseline and verification method when needed. Respect user, project and runtime constraints; where the user has not set a numerical limit, use applicable governing limits and a finite work scope. If a safe execution boundary or the means to observe a required limit is unavailable, stop the affected unattended work rather than claiming that limit can be enforced.

## Progress and scope

- Break consequential work into small, independently verifiable units. Address material uncertainties early; verify each unit before dependent work, retain supported progress and discard or safely undo unsuccessful changes within the owned scope. Do not substitute a worker's self-report for actual evidence or weaken acceptance criteria to make a check pass.
- Resolve necessary subproblems within the authorized goal, including a broken verifier when its repair is authorized. Keep unrelated improvements out of execution; record a distinct task candidate when useful and authorized under [task selection](tasks.md#selection-and-decomposition). Discovery and recording do not grant execution authority.
- Do not stop solely because one approach plateaus. Seek a credible alternative method or evidence path. When two or more attempts based on the same assumption fail the same check, test that assumption directly before another attempt based on it. Treat the number as a diagnostic trigger, not a retry quota or permission to continue beyond limits. Stop when no authorized, credible path to material progress remains. Measure progress against acceptance, not activity: new evidence, resolved uncertainty or verified improvement may count; repeating failed actions without a changed premise does not.

## Exit and continuation

Continue only while there is an authorized, credible path to material progress. Stop or suspend the affected run or work unit when:

- The goal and material acceptance checks are satisfied, or the authorized set of executable work is exhausted. Distinguish full success from an exhausted work set with outstanding goals.
- The user requests a stop, authorization is revoked, or further action requires approval or expansion beyond the authorized scope.
- An applicable time, cost, model-turn, loop or retry limit is reached, or a required runtime capability fails.
- A material dependency, missing evidence, ownership conflict, unsafe side effect or persistent failure prevents trustworthy continuation; repeated attempts without a credible material gain do not justify continuing.

A blocked work unit does not prevent other independently authorized work in the same bounded run when it can safely proceed. Stop the entire run if the blocker invalidates its shared authority, safety, required verification or execution limits; do not start dependent work while its prerequisite is unaccepted. A run-level stop, exhausted shared limit or user cancellation stops further delegation and execution within that run. Follow the applicable project and execution boundaries; do not reset limits by delegation or session changes.

On stopping, start no new work. Where safe and authorized, finish or back out the current atomic step, request termination of delegated executions, verify their actual status and preserve recoverable work without new external side effects. Follow [handoff and resumption](communication.md#publish-and-resume) to record verified progress, remaining work, stop reason, revisions, blockers and next action. If a child cannot be confirmed stopped, report its last verified state and uncertainty, do not start competing work, and do not claim that Hive has stopped it.

Record the run outcome and reason in the existing task or handoff: completion, exhaustion of eligible work, unresolved blocker or saturation, approval wait, limit reached or user stop, as applicable. These are explanations, not new task statuses. Do not mark incomplete work done or equate suspension with task cancellation. A later autonomous run needs renewed user authority; revalidate actual state before resuming. Hive does not enforce runtime timeouts, budgets, termination or scheduling.
