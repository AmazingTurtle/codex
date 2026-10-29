# JavaScript API

Use `workflow.meta({...})` and the `workflow.*` namespace. This runtime does not accept ESM
metadata exports, bare `agent`/`args` globals, imports, or a nested `scriptPath` option.
Scripts have no direct shell, filesystem, network, MCP, or general tool API. Workflow runtime
availability does not enable ordinary JavaScript exec in the parent conversation.

- `await workflow.agent(prompt, options?)` returns an agent's result. JSON final text is parsed;
  other final text remains a string. Prose and Markdown are valid replies. An optional `schema`
  validates structured output when later code needs it. Workers retain normal native tools
  and permissions, including permitted deliverable file writes. An agent failure throws when
  called directly.
- Options are `key`, `label`, `phase`, `schema`, `model`, `reasoningEffort`, `agentType`, and
  `isolation: "worktree"`. A label does not choose a profile. Use an available native profile
  for `agentType`; model, effort, tools and permissions retain native policy. Worktree changes
  require an integration plan; the default shared workspace is simpler for disjoint owners.
- `await workflow.parallel([() => ..., () => ...])` runs functions with a pool of at most
  sixteen concurrent entries, waits for them, and returns results in input order. An agent
  failure makes its entry null; other script errors reject the operation.
- `await workflow.pipeline(items, ...stages)` passes `(previousResult, originalItem, index)` to
  each stage. Items can advance independently, without a whole-stage barrier. An agent failure
  makes that item's result null and skips its remaining stages.
- `await workflow(name, args)` calls a saved workflow by metadata name or scoped ID. One nested
  level is supported. Nested work shares the root run's limits and retains its captured project
  catalog; source reads still use the original caller's permissions.
- `await workflow.phase(title)` selects a declared title; repeated visits are supported.
  `await workflow.log(message)` records concise progress. Await agents and combinators before
  returning; detached work is rejected.
- `workflow.budget.total`, `.spent()`, and `.remaining()` concern total observed tokens for
  this run. An unbounded total or remaining budget is `Infinity`, not null. Use a hard round
  limit even with a finite token cap. The native cap stops new admissions after observed usage
  reaches it; already accepted work may finish.

Keys contain at most 128 UTF-8 bytes and are unique within the full logical scope. Read the
actual scope and logical IDs from `workflow_read`; labels are not identifiers. Source is capped
at 24 KiB. Args, results, and each serialized agent-options object, including its prompt and
schema, are capped at 16 KiB. Default `max_agents`
is 32; retry, schema repair, nested work and revisions all consume that same run allowance.
