import type { AppCtx } from "../main";
import { reveal, rpc } from "../api";
import { ago, baseName, bytes, button, clear, date, errorText, h, num, pill, shortPath, spinner, toast } from "../ui";

interface Project {
  project_id: number; key: string; kind: string; name: string | null; suggested_name: string; root_path: string | null;
  file_count: number; bytes: number; start_ts: number | null; end_ts: number | null; activity_score: number; status: string;
}
interface Detail { project: Project; files: { path: string; mtime: number; size: number }[]; categories: { category: string; files: number }[] }

const displayName = (p: Project) => p.name ?? p.suggested_name;

export async function projectsView(main: HTMLElement, ctx: AppCtx) {
  main.append(h("div", { class: "page-head" }, h("h1", null, "Projects")), spinner());
  const list = await rpc<Project[]>("projects.list", { limit: 200 });
  clear(main);

  const detail = h("div", { class: "card" }, h("div", { class: "empty" }, "Pick a project to see its files."));
  let selected: number | null = ctx.param ? Number(ctx.param) : null;

  async function showProject(id: number) {
    selected = id;
    clear(detail);
    detail.append(spinner());
    try {
      const d = await rpc<Detail>("projects.show", { id, limit: 60 });
      clear(detail);
      const p = d.project;
      const total = d.categories.reduce((a, c) => a + c.files, 0) || 1;
      detail.append(
        h(
          "div",
          { class: "row", style: { marginBottom: "8px" } },
          h("h2", { style: { margin: "0", fontSize: "18px", flex: "1" } }, displayName(p)),
          button("Rename", async () => {
            const name = window.prompt("Project name", displayName(p));
            if (!name) return;
            try {
              await rpc("projects.rename", { id, name });
              toast("Renamed — kept across re-analysis", "ok");
              ctx.go("projects", String(id));
            } catch (e) {
              toast(errorText(e), "error");
            }
          }, { small: true }),
          p.root_path ? button("Reveal folder", () => reveal(p.root_path!), { small: true }) : null,
          button("Search in it", () => ctx.go("search", `in project ${displayName(p)}`), { small: true }),
        ),
        h(
          "div",
          { class: "kv", style: { marginBottom: "12px" } },
          h("span", { class: "k" }, "Kind"), h("span", null, p.kind),
          h("span", { class: "k" }, "Where"), h("span", { class: "path", title: p.root_path ?? "" }, p.root_path ? shortPath(p.root_path) : "spread across folders"),
          h("span", { class: "k" }, "Files"), h("span", null, `${num(p.file_count)} · ${bytes(p.bytes)}`),
          h("span", { class: "k" }, "Active"), h("span", null, `${date(p.start_ts)} → ${date(p.end_ts)} (${ago(p.end_ts)})`),
        ),
        h("div", { class: "row", style: { marginBottom: "12px" } }, d.categories.sort((a, b) => b.files - a.files).map((c) => pill(`${c.category} ${Math.round((c.files / total) * 100)}%`))),
        h(
          "table",
          null,
          h("thead", null, h("tr", null, h("th", null, "File"), h("th", null, "Modified"), h("th", { class: "right" }, "Size"))),
          h("tbody", null, d.files.map((f) => h("tr", { class: "clickable", onClick: () => reveal(f.path), title: f.path }, h("td", null, h("div", null, baseName(f.path)), h("div", { class: "path muted" }, shortPath(f.path.slice(0, f.path.length - baseName(f.path).length)))), h("td", { class: "muted" }, ago(f.mtime)), h("td", { class: "right muted" }, bytes(f.size))))),
        ),
      );
    } catch (e) {
      clear(detail);
      detail.append(h("div", { class: "problem" }, errorText(e)));
    }
  }

  const rows = list.map((p) =>
    h(
      "tr",
      { class: `clickable ${p.project_id === selected ? "active" : ""}`, onClick: () => showProject(p.project_id) },
      h("td", null, h("div", { style: { fontWeight: "600" } }, displayName(p)), h("div", { class: "path muted", title: p.root_path ?? "" }, p.root_path ? shortPath(p.root_path) : p.kind)),
      h("td", { class: "right muted" }, num(p.file_count)),
      h("td", { class: "right muted" }, bytes(p.bytes)),
      h("td", { class: "right muted" }, ago(p.end_ts)),
      h("td", { class: "right" }, h("div", { class: "bar", style: { width: "60px", display: "inline-block" } }, h("i", { style: { width: `${Math.round(p.activity_score * 100)}%` } }))),
    ),
  );

  main.append(
    h("div", { class: "page-head" }, h("h1", null, "Projects"), h("span", { class: "sub" }, `${list.length} detected · most active first`)),
    h(
      "div",
      { class: "grid", style: { gridTemplateColumns: "minmax(380px, 1fr) minmax(420px, 1.2fr)" } },
      h("div", { class: "card", style: { padding: "0" } }, list.length ? h("table", null, h("thead", null, h("tr", null, h("th", null, "Project"), h("th", { class: "right" }, "Files"), h("th", { class: "right" }, "Size"), h("th", { class: "right" }, "Last touched"), h("th", { class: "right" }, "Activity"))), h("tbody", null, rows)) : h("div", { class: "empty" }, "No projects yet — run Re-analyze on the Overview after a scan.")),
      detail,
    ),
  );
  if (selected) showProject(selected);
}
