import { createRoot } from "react-dom/client";
import { Dashboard } from "./dashboard";
import type { ThemePreference } from "./dashboard";
import { parseReport } from "./data/report-data";
import "./styles.css";

const root = document.getElementById("root");
if (root) {
  try {
    const embedded = document.getElementById("report-data");
    if (!embedded?.textContent) throw new Error("Report data is missing");
    const raw: unknown = JSON.parse(embedded.textContent);
    const data = parseReport(raw);
    let theme: ThemePreference = "system";
    try {
      const saved = localStorage.getItem("better-codex-report-theme");
      if (saved === "light" || saved === "dark" || saved === "system")
        theme = saved;
    } catch {
      /* Opening local files may disable browser storage. */
    }
    document.documentElement.dataset.theme =
      theme === "system"
        ? window.matchMedia("(prefers-color-scheme: dark)").matches
          ? "dark"
          : "light"
        : theme;
    createRoot(root).render(<Dashboard data={data} initialTheme={theme} />);
  } catch {
    root.textContent =
      "This report could not be opened because its embedded data is missing or invalid. Generate a fresh report with better-codex report.";
    root.setAttribute("role", "alert");
  }
}
