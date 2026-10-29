# Author a workflow

Pass one-off coordination source directly to `workflow_start`. In an advertised code-mode
tool, this complete call launches inline and prints only the compact run receipt:

```javascript
const source = `
workflow.meta({
  name: "review-change",
  description: "Review a change and summarize actionable findings",
  phases: [{ title: "Review" }, { title: "Summarize" }]
});
await workflow.phase("Review");
const review = await workflow.agent(
  "Review the requested change: " + JSON.stringify(workflow.args),
  { key: "review", label: "Review" }
);
await workflow.phase("Summarize");
return await workflow.agent(
  "Summarize actionable findings as Markdown: " + JSON.stringify(review),
  { key: "summary", label: "Summary" }
);
`;
text(await tools.workflow_start({source, args: {target: "the requested change"}}));
```

The source starts with a literal `workflow.meta({...})` call, reads `workflow.args`, and returns
its final value. Omitted args are `{}`; explicit null stays null. If only the direct function is
advertised, send the same source text and args as its fields. Workflow availability does not
enable a separate general JavaScript tool.

Keep the script inline unless a durable file or reusable saved workflow is wanted. Its source
is already persisted with the run for recovery. Choose a filename-safe metadata `name` to
identify the run; this does not require a file. Saved commands use that name even if the file has
a different name. Declare phase titles before execution. You may revisit a declared phase during
a bounded loop. A phase groups actual discovered work; it is not a fixed checklist or a barrier.
Set `phase` on agents in parallel branches when their progress belongs to different groups.

Drive decisions from agent results. Use a schema for findings that determine branches or repair
owners. Leave `schema` unset when a worker should return a free-form summary or Markdown.
Workers can build deliverables with their normal tools and permissions; asking for a demo page
does not require a JSON reply or forbid file writes. A returned string stays ordinary Markdown.
Give independent verifiers different tasks, such as executing tests and reviewing the
change. Require them to report incomplete checks and omitted coverage. A script that returned
a report has finished running; that alone does not establish that the underlying task passed.

The [repair loop](repair-loop.js) takes a short `task` and disjoint `files.implementation` and
`files.tests` arrays. It requires an approved finite `[dynamic_workflows].max_tokens`, allows
three verification rounds and two repair rounds, and routes findings to the owner of each path.
Use canonical repository-relative paths with forward slashes: no empty, dot or parent segments.
The example accepts simple ASCII paths. Confirm that different paths do not alias the same file
through symlinks, hard links or case folding before assigning ownership.
Each owner gets only its findings and files. An out-of-scope issue or unavailable check blocks
verification. It uses up to eleven logical agent calls when every result passes its schema on
the first attempt; schema repair also consumes the run's shared start and token limits.

Use a fresh keyed agent for a new repair or recheck after files change. Recovery may reuse a
completed call's cached result, and an active agent can receive guidance. Neither is a fresh
verification. Keep requests deterministic so interruption recovery can reuse completed work;
time and randomness are unavailable as coordination inputs.
