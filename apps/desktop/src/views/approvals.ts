import type { AppCtx } from "../main";
import { rpc } from "../api";
import { bytes, button, clear, errorText, h, modal, num, pill, shortPath, spinner, toast } from "../ui";

interface Suggestion { id: number; kind: string; subject: Record<string, unknown>; rationale: string; est_bytes: number; risk_tier: number; state: string }
interface List { proposed: number; est_bytes: number; items: Suggestion[] }
interface Plan { txn_id: string; steps: number; diff: string; problems: string[]; risk_tier: number; mode: string; fingerprint: string }
interface Applied { txn_id: string; done: number; failed: number; state: string }

const KIND_LABEL: Record<string, string> = { trash_duplicates: "Duplicate", trash_duplicate_folder: "Duplicate folder", collapse_versions: "Versions", stale_downloads: "Stale downloads" };

/** Colour the plan's KEEP / TRASH / MOVE lines. */
export function renderDiff(diff: string): HTMLElement {
  const pre = h("pre", { class: "diff" });
  for (const line of diff.split("\n")) {
    const cls = /^\s*KEEP/.test(line) ? "keep" : /^\s*\d+\s+TRASH/.test(line) ? "trash" : /^\s*\d+\s+MOVE|^\s*→/.test(line) ? "move" : "";
    pre.append(h("span", { class: cls }, line.replace(/\/Users\/[^/]+/g, "~")), "\n");
  }
  return pre;
}

export async function approvalsView(main: HTMLElement, ctx: AppCtx) {
  let state = "proposed";
  let kind = "all";
  const body = h("div");
  const head = h("div", { class: "page-head" }, h("h1", null, "Approvals"));

  async function load() {
    clear(body);
    body.append(spinner());
    const [l, mode] = await Promise.all([rpc<List>("suggest.list", { state, limit: 500 }), rpc<string>("mode.get")]);
    clear(body);
    const items = l.items.filter((s) => kind === "all" || s.kind === kind);
    const kinds = Array.from(new Set(l.items.map((s) => s.kind)));

    const filters = h(
      "div",
      { class: "row", style: { marginBottom: "12px" } },
      h("div", { class: "seg" }, ["proposed", "accepted", "dismissed"].map((s) => h("button", { class: s === state ? "active" : "", onClick: () => { state = s; load(); } }, s))),
      h("div", { class: "seg" }, ["all", ...kinds].map((k) => h("button", { class: k === kind ? "active" : "", onClick: () => { kind = k; load(); } }, KIND_LABEL[k] ?? k))),
      h("span", { class: "spacer" }),
      mode === "observe" ? h("span", { class: "muted" }, "Observe mode: you can preview plans, but applying needs Assist mode (Settings).") : null,
    );

    if (items.length === 0) {
      body.append(filters, h("div", { class: "empty" }, state === "proposed" ? "Nothing waiting. Re-analyze on the Overview after new files arrive." : `No ${state} suggestions.`));
      return;
    }

    const rows = items.map((s) => {
      const sub = s.subject;
      const target = (sub.keep as string | undefined) ?? (sub.root as string | undefined) ?? "";
      return h(
        "tr",
        null,
        h("td", null, pill(KIND_LABEL[s.kind] ?? s.kind), " ", pill(`tier ${s.risk_tier}`, `tier${s.risk_tier}`)),
        h("td", null, h("div", null, s.rationale), target ? h("div", { class: "path muted", title: target }, shortPath(target)) : null),
        h("td", { class: "right muted" }, s.est_bytes ? bytes(s.est_bytes) : "—"),
        h(
          "td",
          { class: "right" },
          state === "proposed"
            ? h("div", { class: "row", style: { justifyContent: "flex-end" } }, button("Preview", () => preview(s, mode), { small: true, kind: "primary" }), button("Dismiss", () => dismiss(s), { small: true, kind: "ghost" }))
            : h("span", { class: "muted" }, `#${s.id}`),
        ),
      );
    });
    body.append(filters, h("div", { class: "card", style: { padding: "0" } }, h("table", null, h("thead", null, h("tr", null, h("th", null, "Kind"), h("th", null, "Suggestion"), h("th", { class: "right" }, "Reclaims"), h("th", null))), h("tbody", null, rows))));
  }

  async function dismiss(s: Suggestion) {
    try {
      await rpc("suggest.dismiss", { id: s.id });
      toast("Dismissed — it stays hidden after re-analysis");
      await ctx.refreshNav();
      load();
    } catch (e) {
      toast(errorText(e), "error");
    }
  }

  async function preview(s: Suggestion, mode: string) {
    const content = h("div", null, spinner("Planning — nothing is touched yet…"));
    const m = modal(`Suggestion #${s.id}`, content);
    try {
      const plan = await rpc<Plan>("suggest.plan", { id: s.id });
      clear(content);
      content.append(
        h("p", null, s.rationale),
        h("div", { class: "row", style: { marginBottom: "10px" } }, pill(`${num(plan.steps)} step${plan.steps === 1 ? "" : "s"}`), pill(`risk tier ${plan.risk_tier}`, `tier${plan.risk_tier}`), s.est_bytes ? pill(`reclaims ${bytes(s.est_bytes)}`) : null, h("span", { class: "muted mono" }, plan.txn_id)),
        renderDiff(plan.diff),
        ...plan.problems.map((p) => h("div", { class: "problem" }, p)),
        h("p", { class: "muted", style: { fontSize: "12px" } }, "Trashed files go to the Trash; moves never overwrite. The whole transaction is journaled first and can be undone from History, with every file's contents verified before it is put back."),
      );
      const actions = h(
        "div",
        { class: "modal-actions" },
        button("Cancel", () => m.close()),
        button(
          plan.problems.length ? "Cannot apply" : mode === "observe" ? "Switch to Assist to apply" : `Apply ${plan.steps} step${plan.steps === 1 ? "" : "s"}`,
          async () => {
            if (plan.problems.length) return;
            if (mode === "observe") {
              m.close();
              ctx.go("settings");
              return;
            }
            try {
              const r = await rpc<Applied>("suggest.apply", { id: s.id, approved: true, txn_id: plan.txn_id, fingerprint: plan.fingerprint });
              m.close();
              toast(r.failed ? `${r.done} done, ${r.failed} failed — see History` : `Done: ${r.done} step${r.done === 1 ? "" : "s"}. Undo from History.`, r.failed ? "error" : "ok");
              await ctx.refreshNav();
              load();
            } catch (e) {
              toast(errorText(e), "error");
            }
          },
          { kind: plan.problems.length ? undefined : "primary" },
        ),
      );
      content.parentElement?.parentElement?.append(actions);
    } catch (e) {
      clear(content);
      content.append(h("div", { class: "problem" }, errorText(e)));
    }
  }

  main.append(head, body);
  await load();
  const l = await rpc<List>("suggest.list", { limit: 0 });
  head.append(h("span", { class: "sub" }, `${num(l.proposed)} proposed · ${bytes(l.est_bytes)} reclaimable`));
}
