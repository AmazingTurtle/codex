# Control, save, and recover

Read `workflow_list`, `workflow_read`, and `workflow_control` action `read` before changing work.
Keep the observed control revision and exact logical step, latest attempt and current owned
turn identities. An absent current owned turn makes active guidance or interruption unavailable.
Do not infer it from the last recorded turn or identify work by its display label.

`workflow_control` mutations require `run_id`, `command_id`, `expected_control_revision` and
`target`. `guide` and `interrupt` require a `step` with `logical_id`, `expected_attempt_id` and
`expected_turn_id`. `edit_queued` and `skip` require an exact step or a `future_step` with its
full `scope` and `key`. Run and phase mutations are rejected. Skip defaults to null; a supplied
replacement must satisfy the step's schema. Check its receipt rather than assuming it ran.

Guidance clarifies an agent's current accepted work. For a new repair after verification, use
a fresh keyed call. Keep a command ID stable when delivery is uncertain and inspect history
before resubmitting. A stale guard is a conflict. Mutations use the starting agent's current
permissions; another agent cannot take over the run.

For a changed plan, read explicit `workflow_read` sections `source` and `args`, following
`next_offset`. An optional exact `execution_id` selects a known historical execution. Stage
canonical source with `workflow_revision` action `stage`; omitted `args` retains the current
value and explicit null stays null. Inspect the reusable prefix and invalidation reason before
`apply`. Default `drain_accepted` fences new starts and lets accepted work finish.
`interrupt_and_apply` requests interruption. Resume only the pending Ready revision. Revisions
do not reset budgets; results and source from earlier executions remain inspectable.

Use `workflow_retry` for failed or interrupted execution whose source and replayed requests
should remain unchanged. Inspect and fix its external cause first. Completed calls may be
reused. `workflow_stop` stops further progress. Existing V2 runs remain manageable in Off mode.
Older scripts and runs are read-only; author a new V2 script only when migration is requested.

`/pause` pauses the conversation, including its agents and workflows; already running tools may finish.
From a worker or the workflow center, confirm that conversation-wide scope.
Agent messages and background wake signals queue without resuming the conversation.
Use `/unpause` or send a new human message to resume; an idle conversation has nothing to pause.
These are human TUI commands, not workflow JavaScript methods.

For intended reuse, use `workflow_save` to save a run's source in project or user scope.
One-off inline runs already retain source for recovery. Keep an existing user script file when
requested; do not create a temporary file merely to launch it. Default the saved filename to
the metadata name; a different filename does not change the command name. Inspect saved scripts
with `saved_workflow_list` and `saved_workflow_read`. Project aliases take precedence; a scoped
ID resolves a collision. Saving or reading a script grants no additional delegation permission.
Read final results with `workflow_read` section `result` only when the task needs analysis;
the normal human history display does not require a model follow-up.
