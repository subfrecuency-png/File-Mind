import { agentInfo, rpc } from "./api";
import { resumeRunningJobs } from "./jobs";
import { clear, h, pill } from "./ui";
import { homeView } from "./views/home";
import { searchView } from "./views/search";
import { projectsView } from "./views/projects";
import { approvalsView } from "./views/approvals";
import { historyView } from "./views/history";
import { automateView } from "./views/automate";
import { settingsView } from "./views/settings";
import { onboardingView } from "./views/onboarding";

export type View = (main: HTMLElement, ctx: AppCtx) => Promise<void> | void;

export interface AppCtx {
  go: (route: string, param?: string) => void;
  refreshNav: () => Promise<void>;
  param?: string;
}

const routes: Record<string, { label: string; icon: string; view: View }> = {
  home: { label: "Overview", icon: "◎", view: homeView },
  search: { label: "Search", icon: "⌕", view: searchView },
  projects: { label: "Projects", icon: "▤", view: projectsView },
  approvals: { label: "Approvals", icon: "✓", view: approvalsView },
  automate: { label: "Automate", icon: "▶", view: automateView },
  history: { label: "History", icon: "↺", view: historyView },
  settings: { label: "Settings", icon: "⚙", view: settingsView },
};

const app = document.getElementById("app")!;
const sidebar = h("aside", { class: "sidebar" });
const main = h("main");
app.append(sidebar, main);

let current = "home";
let currentParam: string | undefined;
const counts: Record<string, number | undefined> = {};
let modeLabel = "";
let agent = { running: false, stale: false };

function go(route: string, param?: string) {
  current = routes[route] ? route : "home";
  currentParam = param;
  location.hash = param ? `${current}/${encodeURIComponent(param)}` : current;
  render();
}

async function refreshNav() {
  try {
    const [s, st] = await Promise.all([
      rpc<{ proposed: number }>("suggest.list", { limit: 0 }),
      rpc<{ mode: string }>("status"),
    ]);
    counts.approvals = s.proposed;
    modeLabel = st.mode;
  } catch {
    /* offline: leave counts */
  }
  try {
    const a = await agentInfo();
    agent = { running: a.running, stale: a.stale };
  } catch {
    agent = { running: false, stale: false };
  }
  renderSidebar();
}

function renderSidebar() {
  clear(sidebar);
  sidebar.append(
    h("div", { class: "brand" }, h("span", { class: "logo" }, "F"), "FileMind"),
    h(
      "nav",
      { class: "nav" },
      Object.entries(routes).map(([k, r]) =>
        h(
          "a",
          { class: k === current ? "active" : "", onClick: () => go(k) },
          h("span", null, r.icon),
          r.label,
          counts[k] ? h("span", { class: "count" }, String(counts[k])) : null,
        ),
      ),
    ),
    h(
      "div",
      { class: "foot" },
      h("div", null, h("span", { class: `agent-dot ${agent.running ? "on" : agent.stale ? "stale" : ""}` }), agent.running ? "agent running" : agent.stale ? "agent from an older build" : "agent not running"),
      h("div", { style: { marginTop: "6px" } }, "mode ", modeLabel ? pill(modeLabel, `mode-${modeLabel}`) : "…"),
    ),
  );
}

async function render() {
  renderSidebar();
  document.querySelectorAll(".overlay").forEach((o) => o.remove()); // a modal never outlives its screen
  clear(main);
  const ctx: AppCtx = { go, refreshNav, param: currentParam };
  try {
    await routes[current].view(main, ctx);
  } catch (e) {
    clear(main);
    main.append(h("div", { class: "empty" }, `Could not load this screen: ${String(e)}`));
  }
}

async function boot() {
  // first run: no roots yet → onboarding
  let roots: string[] = [];
  try {
    roots = await rpc<string[]>("roots.list");
  } catch {
    /* fallthrough */
  }
  const [route, param] = location.hash.replace(/^#/, "").split("/");
  if ((roots.length === 0 && route !== "settings") || route === "onboarding") {
    clear(main);
    await onboardingView(main, { go, refreshNav });
    await refreshNav();
    return;
  }
  current = routes[route] ? route : "home";
  currentParam = param ? decodeURIComponent(param) : undefined;
  await refreshNav();
  await render();
  void resumeRunningJobs();
  setInterval(refreshNav, 15000);
}

window.addEventListener("hashchange", () => {
  const [route, param] = location.hash.replace(/^#/, "").split("/");
  if (routes[route] && (route !== current || (param ? decodeURIComponent(param) : undefined) !== currentParam)) {
    current = route;
    currentParam = param ? decodeURIComponent(param) : undefined;
    render();
  }
});

boot();
