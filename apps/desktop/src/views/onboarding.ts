import type { AppCtx } from "../main";
import { pickFolder, rpc } from "../api";
import { button, clear, errorText, h, shortPath, toast } from "../ui";

const RULES: [string, string][] = [
  ["Never deletes", "Files only ever go to the Trash, where you can put them back."],
  ["Never moves in bulk without a plan", "Every action is a transaction you preview, approve and can undo."],
  ["Never overwrites", "A destination that already exists stops the move; nothing is renamed silently."],
  ["Never follows unknown links", "Symlinks and junctions are left alone."],
  ["Never touches system files", "Library, Applications and app internals are off limits."],
  ["Stays on your Mac", "Indexing and search are local. Cloud AI is opt-in and audited."],
];

export async function onboardingView(main: HTMLElement, ctx: AppCtx) {
  let roots: string[] = [];
  let mode = "observe";

  const rootsList = h("div");
  const modeBox = h("div");

  function renderRoots() {
    clear(rootsList);
    if (roots.length === 0) rootsList.append(h("div", { class: "muted", style: { padding: "8px 0" } }, "No folders yet. Downloads and Documents are the usual place to start."));
    for (const r of roots) {
      rootsList.append(
        h("div", { class: "row", style: { padding: "6px 0" } }, h("span", { class: "path", style: { flex: "1" } }, shortPath(r)), button("Remove", () => { roots = roots.filter((x) => x !== r); renderRoots(); }, { kind: "ghost", small: true })),
      );
    }
  }
  function renderMode() {
    clear(modeBox);
    const opts: [string, string, string][] = [
      ["observe", "Observe", "FileMind only watches, indexes and suggests. It never moves a file."],
      ["assist", "Assist", "Suggestions can be applied — each one after you approve its plan. Everything is undoable."],
    ];
    for (const [k, title, desc] of opts) {
      modeBox.append(
        h("div", { class: `mode-opt ${mode === k ? "active" : ""}`, onClick: () => { mode = k; renderMode(); } }, h("span", null, mode === k ? "●" : "○"), h("div", null, h("b", null, title), h("span", null, desc))),
      );
    }
  }
  renderRoots();
  renderMode();

  main.append(
    h(
      "div",
      { class: "onboard" },
      h("h1", null, "Welcome to FileMind"),
      h("p", { class: "muted" }, "It finds what you are looking for, spots duplicates and stale downloads, and only ever changes something when you say so."),
      h("div", { class: "rules" }, RULES.map(([t, d]) => h("div", { class: "rule" }, h("b", null, t), d))),
      h(
        "div",
        { class: "card" },
        h("h3", null, "1. Folders to index"),
        rootsList,
        h("div", { style: { marginTop: "8px" } }, button("Add a folder…", async () => {
          const p = await pickFolder();
          if (p && !roots.includes(p)) {
            roots.push(p);
            renderRoots();
          }
        })),
      ),
      h("div", { class: "card" }, h("h3", null, "2. How much should it do?"), modeBox),
      h(
        "div",
        { class: "row", style: { marginTop: "18px", justifyContent: "flex-end" } },
        button("Start indexing", async () => {
          if (roots.length === 0) {
            toast("Add at least one folder", "error");
            return;
          }
          try {
            for (const r of roots) await rpc("roots.add", { path: r });
            await rpc("mode.set", { mode });
            toast("Scanning in the background…", "ok");
            rpc("scan").catch(() => undefined);
            await ctx.refreshNav();
            ctx.go("home");
          } catch (e) {
            toast(errorText(e), "error");
          }
        }, { kind: "primary" }),
      ),
    ),
  );
}
