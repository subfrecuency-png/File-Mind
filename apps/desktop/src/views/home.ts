import type { AppCtx } from "../main";
import { rpc } from "../api";
import { runJob } from "../jobs";
import { ago, bytes, button, clear, errorText, h, num, pill, shortPath, spinner, toast } from "../ui";

interface Status {
  mode: string; uptime_s: number; files: number; dirs: number; missing: number; hashed: number; bytes: number; events: number; transactions: number;
  roots: { path: string; last_scan: number | null }[];
  watcher: { roots: number; raw_events: number; changes_applied: number; last_change_unix: number };
}
interface Health {
  health: { score: number; components: { name: string; ratio: number; weight: number; penalty: number; detail: string }[] };
  roots: { path: string; score: number }[];
  history: [number, number][];
}
interface Categories { categories: { category: string; files: number; bytes: number }[]; sensitive: number; pending: number }
interface Embed { model: string; semantic: boolean; vectors: number; embedded: number; pending: number; model_installed: boolean }
interface Suggest { proposed: number; est_bytes: number }
interface ShrinkTier { tier: number; kind: string; label: string; measured: boolean; candidate_files: number; candidate_bytes: number; saving_bytes: number; projects?: { name: string; saving_bytes: number }[] }
interface ShrinkEstimate { computed_ts: number; files_seen: number; sampled_files: number; saving_bytes: number; tiers: ShrinkTier[] }
interface Shrink { estimate: ShrinkEstimate | null; cached: boolean }

const CAT_COLORS: Record<string, string> = {
  photo: "#5b9cf6", code: "#7c6cf0", document: "#3bbf8a", media: "#f0a64a", design: "#e66aa4", invoice: "#f2c94c",
  contract: "#8bd17c", archive: "#9aa5b5", installer: "#b0b8c4", data: "#4fc3d9", screenshot: "#a5c8ff", other: "#c9cfd8",
};

function scoreColor(s: number) {
  return s >= 80 ? "var(--ok)" : s >= 60 ? "var(--warn)" : "var(--danger)";
}

function sparkline(points: [number, number][]) {
  const svgNS = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(svgNS, "svg");
  svg.setAttribute("class", "sparkline");
  svg.setAttribute("viewBox", "0 0 300 48");
  svg.setAttribute("preserveAspectRatio", "none");
  const pts = [...points].sort((a, b) => a[0] - b[0]);
  if (pts.length < 2) return svg;
  const xs = pts.map((p) => p[0]);
  const min = Math.min(...xs);
  const max = Math.max(...xs) || min + 1;
  const d = pts.map((p, i) => `${i === 0 ? "M" : "L"}${((p[0] - min) / (max - min || 1)) * 300},${48 - (p[1] / 100) * 44 - 2}`).join(" ");
  const path = document.createElementNS(svgNS, "path");
  path.setAttribute("d", d);
  path.setAttribute("fill", "none");
  path.setAttribute("stroke", "var(--accent)");
  path.setAttribute("stroke-width", "2");
  svg.appendChild(path);
  return svg;
}

function shrinkLine(est: ShrinkEstimate): string {
  const t = (k: string) => est.tiers.find((x) => x.kind === k);
  const parts: string[] = [];
  const a = t("apfs");
  if (a && a.saving_bytes > 0) parts.push(`${bytes(a.saving_bytes)} code/text via APFS`);
  const m = t("media_lossless");
  if (m && m.saving_bytes > 0) parts.push(`~${bytes(m.saving_bytes)} photos (lossless)`);
  const c = t("cold_archive");
  if (c && c.saving_bytes > 0) parts.push(`${bytes(c.saving_bytes)} in ${num(c.projects?.length ?? 0)} cold project${(c.projects?.length ?? 0) === 1 ? "" : "s"}`);
  return parts.length ? parts.join(" · ") : "nothing worth reclaiming right now";
}

/** "Shrinkable": what lossless compression could reclaim. Measured by sampling; nothing is rewritten. */
function shrinkTile(est: ShrinkEstimate | null, ctx: AppCtx): HTMLElement {
  const run = async () => {
    try {
      const r = await runJob<Shrink>("shrink.estimate");
      toast(`Shrink could reclaim ~${bytes(r.estimate?.saving_bytes ?? 0)}`, "ok");
      ctx.go("home");
    } catch (e) {
      toast(errorText(e), "error");
    }
  };
  return h(
    "div",
    { class: "card" },
    h("h3", null, "Shrinkable"),
    h("div", { class: "stat" }, est ? bytes(est.saving_bytes) : "—"),
    h("div", { class: "muted" }, est ? shrinkLine(est) : "measure what lossless compression could reclaim — reads a few KB per file, changes nothing"),
    h(
      "div",
      { style: { marginTop: "12px" }, class: "row" },
      button(est ? "Re-estimate" : "Estimate", run, { kind: est ? "ghost" : "primary" }),
      est ? h("span", { class: "muted", style: { fontSize: "12px" } }, `measured ${ago(est.computed_ts)}`) : null,
    ),
  );
}

export async function homeView(main: HTMLElement, ctx: AppCtx) {
  main.append(h("div", { class: "page-head" }, h("h1", null, "Overview")), spinner());
  const [st, he, cat, emb, sug, shr] = await Promise.all([
    rpc<Status>("status"),
    rpc<Health>("health"),
    rpc<Categories>("categories"),
    rpc<Embed>("embed.status").catch(() => null),
    rpc<Suggest>("suggest.list", { limit: 0 }),
    rpc<Shrink>("shrink.estimate", { cached_only: true }).catch(() => null),
  ]);
  clear(main);

  const score = he.health.score;
  const ring = h("div", { class: "ring", style: { background: `conic-gradient(${scoreColor(score)} ${score * 3.6}deg, var(--panel-2) 0)` } }, h("span", null, String(score)));
  const comps = he.health.components
    .slice()
    .sort((a, b) => b.penalty - a.penalty)
    .map((c) =>
      h(
        "div",
        { class: "comp" },
        h("div", null, c.name.replace(/_/g, " "), h("div", { class: "detail" }, c.detail)),
        h("div", { class: `bar ${c.ratio > 0.5 ? "danger" : c.ratio > 0.2 ? "warn" : ""}` }, h("i", { style: { width: `${Math.round(c.ratio * 100)}%` } })),
        h("div", { class: "right muted" }, `−${c.penalty.toFixed(1)}`),
      ),
    );

  const totalFiles = cat.categories.reduce((a, c) => a + c.files, 0) || 1;
  const catBar = h("div", { class: "cat-bar" }, cat.categories.map((c) => h("i", { style: { width: `${(c.files / totalFiles) * 100}%`, background: CAT_COLORS[c.category] ?? "#ccc" }, title: `${c.category}: ${num(c.files)} files` })));
  const legend = h("div", { class: "legend" }, cat.categories.slice(0, 8).map((c) => h("span", null, h("i", { style: { background: CAT_COLORS[c.category] ?? "#ccc" } }), `${c.category} ${num(c.files)}`)));

  const actions = h(
    "div",
    { class: "row" },
    button("Scan now", async () => {
      try {
        const r = await runJob<{ files: number; elapsed_ms: number }[]>("scan");
        toast(`Scanned ${num(r.reduce((a, x) => a + x.files, 0))} files`, "ok");
        ctx.go("home");
      } catch (e) {
        toast(errorText(e), "error");
      }
    }),
    button("Re-analyze", async () => {
      try {
        const r = await runJob<{ suggestions: number; duplicate_groups: number }>("analyze");
        toast(`${num(r.duplicate_groups)} duplicate groups, ${num(r.suggestions)} suggestions`, "ok");
        await ctx.refreshNav();
        ctx.go("home");
      } catch (e) {
        toast(errorText(e), "error");
      }
    }),
  );

  main.append(
    h("div", { class: "page-head" }, h("h1", null, "Overview"), h("span", { class: "sub" }, `${num(st.files)} files · ${bytes(st.bytes)} across ${st.roots.length} folder${st.roots.length === 1 ? "" : "s"}`), h("span", { class: "spacer" }), actions),
    h(
      "div",
      { class: "grid cols-4" },
      h(
        "div",
        { class: "card" },
        h("h3", null, "Health"),
        h("div", { class: "score-ring" }, ring, h("div", null, h("div", { class: "stat" }, score, h("small", null, "/ 100")), h("div", { class: "muted", style: { fontSize: "12px" } }, score >= 80 ? "Tidy. Keep it that way." : score >= 60 ? "Some clutter worth a look." : "Plenty to reclaim — see Approvals."))),
        sparkline(he.history),
      ),
      h(
        "div",
        { class: "card" },
        h("h3", null, "Reclaimable"),
        h("div", { class: "stat" }, bytes(sug.est_bytes)),
        h("div", { class: "muted" }, `${num(sug.proposed)} suggestions waiting for your approval`),
        h("div", { style: { marginTop: "12px" } }, button("Review approvals", () => ctx.go("approvals"), { kind: "primary" })),
      ),
      shrinkTile(shr?.estimate ?? null, ctx),
      h(
        "div",
        { class: "card" },
        h("h3", null, "Index"),
        h(
          "div",
          { class: "kv" },
          h("span", { class: "k" }, "Files"), h("span", null, num(st.files)),
          h("span", { class: "k" }, "Hashed"), h("span", null, `${num(st.hashed)} (${Math.round((st.hashed / Math.max(1, st.files)) * 100)}%)`),
          h("span", { class: "k" }, "Classified"), h("span", null, cat.pending ? `${num(cat.pending)} pending` : "up to date"),
          h("span", { class: "k" }, "Vectors"), h("span", null, emb ? (emb.semantic ? `${num(emb.vectors)}${emb.pending ? ` (+${num(emb.pending)} pending)` : ""}` : "model not installed") : "—"),
          h("span", { class: "k" }, "Sensitive"), h("span", null, cat.sensitive ? pill(`${num(cat.sensitive)} files`, "sensitive") : "none found"),
          h("span", { class: "k" }, "Watcher"), h("span", null, st.watcher.roots ? `${num(st.watcher.changes_applied)} changes, last ${ago(st.watcher.last_change_unix)}` : "not running"),
        ),
      ),
    ),
    h(
      "div",
      { class: "grid cols-2", style: { marginTop: "14px" } },
      h("div", { class: "card" }, h("h3", null, "What is costing points"), comps),
      h(
        "div",
        { class: "card" },
        h("h3", null, "What is in here"),
        catBar,
        legend,
        h("h3", { style: { marginTop: "18px" } }, "Folders"),
        h("table", null, h("tbody", null, he.roots.map((r) => {
          const rs = st.roots.find((x) => x.path === r.path);
          return h("tr", null, h("td", { class: "path", title: r.path }, shortPath(r.path)), h("td", { class: "muted" }, rs?.last_scan ? `scanned ${ago(rs.last_scan)}` : "not scanned"), h("td", { class: "right", style: { color: scoreColor(r.score), fontWeight: "600" } }, String(r.score)));
        }))),
      ),
    ),
  );
}
