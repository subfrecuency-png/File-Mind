import type { AppCtx } from "../main";
import { rpc } from "../api";
import { ago, button, clear, errorText, h, modal, pill, spinner, toast } from "../ui";
import { renderDiff } from "./approvals";

interface Txn { txn_id: string; state: string; created_ts: number; executed_ts: number | null; rationale: string; steps: number; done: number }
interface Shown { manifest: { rationale: string }; state: string; steps: string[]; diff: string }
interface Undone { txn_id: string; restored: number; skipped: string[] }

export async function historyView(main: HTMLElement, ctx: AppCtx) {
  main.append(h("div", { class: "page-head" }, h("h1", null, "History")), spinner());
  const txns = await rpc<Txn[]>("txn.list", { limit: 200 });
  clear(main);

  async function show(t: Txn) {
    const content = h("div", null, spinner());
    const m = modal(t.txn_id, content);
    try {
      const s = await rpc<Shown>("txn.show", { id: t.txn_id });
      clear(content);
      content.append(h("p", null, s.manifest.rationale), h("div", { class: "row", style: { marginBottom: "10px" } }, pill(s.state, s.state), pill(`${s.steps.filter((x) => x === "done").length}/${s.steps.length} done`)), renderDiff(s.diff));
      if (s.state === "done") {
        content.parentElement?.parentElement?.append(h("div", { class: "modal-actions" }, button("Close", () => m.close()), button("Undo this transaction", () => { m.close(); undo(t); }, { kind: "danger" })));
      }
    } catch (e) {
      clear(content);
      content.append(h("div", { class: "problem" }, errorText(e)));
    }
  }

  async function undo(t: Txn) {
    try {
      const r = await rpc<Undone>("txn.undo", { id: t.txn_id });
      toast(r.skipped.length ? `${r.restored} restored, ${r.skipped.length} left alone (edited or occupied)` : `${r.restored} file${r.restored === 1 ? "" : "s"} put back`, r.skipped.length ? "info" : "ok");
      if (r.skipped.length) {
        modal("Left alone", h("div", null, h("p", null, "These were not moved back because something changed since the transaction ran:"), h("ul", null, r.skipped.map((s) => h("li", { class: "mono" }, s)))));
      }
      ctx.go("history");
    } catch (e) {
      toast(errorText(e), "error");
    }
  }

  const rows = txns.map((t) =>
    h(
      "tr",
      { class: "clickable", onClick: () => show(t) },
      h("td", { class: "muted" }, ago(t.executed_ts ?? t.created_ts)),
      h("td", null, pill(t.state, t.state)),
      h("td", null, h("div", null, t.rationale), h("div", { class: "mono muted" }, t.txn_id)),
      h("td", { class: "right muted" }, `${t.done}/${t.steps}`),
      h("td", { class: "right" }, t.state === "done" ? button("Undo", (e?: Event) => { (e as Event | undefined)?.stopPropagation?.(); undo(t); }, { small: true }) : null),
    ),
  );

  main.append(
    h("div", { class: "page-head" }, h("h1", null, "History"), h("span", { class: "sub" }, "every change FileMind has made, newest first — each one can be undone")),
    h("div", { class: "card", style: { padding: "0" } }, txns.length ? h("table", null, h("thead", null, h("tr", null, h("th", null, "When"), h("th", null, "State"), h("th", null, "What"), h("th", { class: "right" }, "Steps"), h("th", null))), h("tbody", null, rows)) : h("div", { class: "empty" }, "No transactions yet. Nothing has been moved.")),
  );
}
