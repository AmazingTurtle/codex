workflow.meta({
  name: "bounded-change",
  description: "Implement a change, verify it, and repair the responsible owners' files",
  phases: [{ title: "Implement" }, { title: "Verify" }, { title: "Repair" }]
});
const args = workflow.args;
const owners = ["implementation", "tests"];
if (!args || typeof args.task !== "string" || !args.task || args.task.length > 1000) {
  throw new Error("args.task must contain 1..1000 characters");
}
if (!owners.every(owner => Array.isArray(args.files?.[owner]) && args.files[owner].length <= 2 &&
    args.files[owner].every(path => typeof path === "string" && path.length <= 80 &&
      /^[A-Za-z0-9._/-]+$/.test(path) && path.split("/").every(part => part !== "" && part !== "." && part !== "..")))) {
  throw new Error("Provide up to two canonical repository-relative file paths for each owner");
}
const files = Object.fromEntries(owners.map(owner => [owner, args.files[owner]]));
const paths = owners.flatMap(owner => files[owner]);
const fileOwner = new Map(owners.flatMap(owner => files[owner].map(path => [path, owner])));
if (paths.length === 0 || fileOwner.size !== paths.length) throw new Error("File ownership must be disjoint");
if (!Number.isFinite(workflow.budget.total)) throw new Error("This example requires an approved finite max_tokens");
const summarySchema = {
  type: "object", additionalProperties: false, required: ["summary"],
  properties: { summary: { type: "string", maxLength: 160 } }
};
const reportSchema = {
  type: "object", additionalProperties: false, required: ["complete", "blocked", "checks", "findings"],
  properties: {
    complete: { type: "boolean" }, blocked: { type: "string", maxLength: 120 },
    checks: { type: "array", maxItems: 4, items: { type: "string", maxLength: 120 } },
    findings: { type: "array", maxItems: 2, items: {
      type: "object", additionalProperties: false, required: ["path", "issue", "check"],
      properties: { path: { type: "string", enum: paths }, issue: { type: "string", maxLength: 100 }, check: { type: "string", maxLength: 60 } }
    } }
  }
};
await workflow.phase("Implement");
const implemented = await workflow.agent(
  "Implement this task within the listed files. Respect repository instructions and existing edits. Task: " +
  args.task + "\nOwnership: " + JSON.stringify(files),
  { key: "implement", label: "Implement", phase: "Implement", schema: summarySchema }
);
for (let round = 0; round < 3; round++) {
  if (workflow.budget.remaining() <= 0) return { status: "budget-exhausted" };
  await workflow.phase("Verify");
  const context = "\nTask: " + args.task + "\nFiles: " + JSON.stringify(paths) + "\nChange: " + implemented.summary;
  const report = " Do not edit. Record checks and evidence. Report up to two findings; set complete false if coverage or findings remain omitted. Explain missing tools, permissions or out-of-scope issues in blocked.";
  const reports = await workflow.parallel([
    () => workflow.agent("QA round " + round + ": Run relevant repository checks." + report + context,
      { key: "qa-" + round, label: "QA", phase: "Verify", schema: reportSchema }),
    () => workflow.agent("Review round " + round + ": Independently review correctness and missing cases." + report + context,
      { key: "review-" + round, label: "Review", phase: "Verify", schema: reportSchema })
  ]);
  if (reports.some(value => value === null)) return { status: "blocked", reason: "A verifier failed" };
  const blocked = reports.map(value => value.blocked).filter(Boolean);
  if (blocked.length) return { status: "blocked", reasons: blocked };
  const findings = reports.flatMap(value => value.findings);
  const checks = reports.flatMap(value => value.checks);
  if (findings.length === 0) {
    return reports.every(value => value.complete)
      ? { status: "verified", checks, summary: implemented.summary }
      : { status: "needs-attention", reason: "Verification coverage is incomplete", checks };
  }
  if (round === 2) return { status: "needs-attention", findings, checks };
  await workflow.phase("Repair");
  const repairs = await workflow.parallel(owners.filter(owner => findings.some(value => fileOwner.get(value.path) === owner)).map(owner =>
    () => workflow.agent("Repair only the listed files for " + owner + ". Preserve others' edits. Files: " +
      JSON.stringify(files[owner]) + "\nFindings: " + JSON.stringify(findings.filter(value => fileOwner.get(value.path) === owner)),
      { key: "repair-" + round + "-" + owner, label: "Repair " + owner, phase: "Repair" })
  ));
  if (repairs.some(value => value === null)) return { status: "blocked", reason: "An owner repair failed" };
}
