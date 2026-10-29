---
name: dynamic-workflow
description: Author and manage agent workflows for independent work, result-driven decisions, verification, and bounded repair cycles. Use within the user's and repository's delegation rules.
---

# Dynamic workflows

Use a workflow when coordination needs branching, repeated checks, independent agents, or work
that should survive interruption. For a small focused task, direct tools may be enough. Scout
first if the worklist or file ownership is still unclear. Follow a user's explicit request to
use a workflow when delegation is allowed.

In `auto` mode you may choose useful, authorized coordination. In `explicit` mode, a human must
invoke `$dynamic-workflow` or select a saved workflow. In `off` mode, inspect and control existing
runs without starting another. Reading these instructions through a tool is not authorization.
If the user asks only to author, explain, or review a script, do that without launching it.

Read [authoring](references/authoring.md) before writing source, and use the
[API reference](references/api.md) for exact semantics. For implementation work, adapt the
[bounded verification and repair example](references/repair-loop.js): implement, run QA and
review, repair only the responsible owners' files, then recheck with fresh agents.

Inspect existing runs before launching another. Use `workflow_start` with exactly one of
`source` or `saved_workflow_id`; `args` defaults to `{}` and preserves explicit null.
For a one-off run, pass new source inline. Use `tools.workflow_start({source, args})` when the
advertised code-mode tool is available. If `workflow_start` is advertised as a direct function,
send the source there. Skip temporary script creation, readback, and source dumps.
The runtime persists source for recovery.
Create a workflow file or saved library entry when the user wants durable source or reuse.

Scripts coordinate agents; workers use their normal tools and permissions to create requested
deliverables, including pages and source files. Their replies may be prose or Markdown.
Use an optional `schema` when later code needs validated facts for branching or verification.

Use [controls and recovery](references/controls.md) to guide active work, revise a plan, save
source, or recover interruption. Keep the original agent and token limits. Results appear in
conversation history by default; do not poll for or automatically repeat the full report.
Report unresolved checks honestly, even when the script itself completed.
