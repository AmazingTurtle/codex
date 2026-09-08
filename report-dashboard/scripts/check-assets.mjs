import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve, join, dirname } from "node:path";
import { createRequire } from "node:module";
import { spawnSync } from "node:child_process";

const require = createRequire(import.meta.url);
const temporary = await mkdtemp(join(tmpdir(), "better-codex-assets-"));
try {
  const vite = join(
    dirname(require.resolve("vite/package.json")),
    "bin/vite.js",
  );
  const build = spawnSync(
    process.execPath,
    [vite, "build", "--outDir", temporary],
    { stdio: "inherit" },
  );
  if (build.status !== 0) throw new Error("Dashboard build failed");
  for (const name of ["dashboard.js", "dashboard.css"]) {
    const [generated, committed] = await Promise.all([
      readFile(join(temporary, name)),
      readFile(resolve("../codex-rs/report/assets", name)),
    ]);
    if (!generated.equals(committed))
      throw new Error(
        `${name} is stale; run pnpm --filter @better-codex/report-dashboard build`,
      );
  }
  console.log("Dashboard assets match their sources.");
} finally {
  await rm(temporary, { recursive: true, force: true });
}
