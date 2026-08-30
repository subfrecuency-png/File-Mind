import type { AppCtx } from "../main";
import { rpc } from "../api";
import { ago, bytes, button, clear, errorText, h, modal, num, pill, shortPath, spinner, toast } from "../ui";
import { renderDiff } from "./approvals";

// Automate mode (Phase 9). A rule previews for a week — every agent tick
// records what it *would* have done — and only then can be armed. Armed
// rules execute only while the mode is Automate; each run is a normal
// transaction in History with undo. Any conflict pauses the rule.

interface Rule { rule_id: number; kind: string; params: Record<string, unknown>; tier: number; state: "preview" | "armed" | "paused"; created_ts: number; armed_ts: number | null; paused_ts: number | null; paused_reason: string | null }
interface WouldHave { rule_id: number; since_ts: number; dry_runs: number; real_runs: number; files: string[]; bytes: number; last_eval_ts: number | null; last_problems: string[]; armable: boolean; armable_in_secs: number; armable_reason: string }
interface Listed { rule: Rule; describe: string; would_have: WouldHave }
interface List { mode: string; preview_days: number; rules: Listed[] }
interface KindInfo { kind: string; defaults: Record<string, unknown>; describe: string }
interface Preview { rule: Rule; now: { candidates: number; candidate_bytes: number; steps: number; bytes: number; capped: boolean; problems: string[]; diff: string }; keeps: string[]; would_have: WouldHave }

const KIND_LABEL: Record<string, string> = { archive_stale_downloads: "Archive stale downloads", collapse_versions: "Collapse versions", trash_exact_duplicates: "Trash exact duplicates" };
const PARAM_LABEL: Record<string, string> = {
  older_than_days: "Untouched for at least (days)",
  max_items_per_run: "Files per run (max)",
  pause_above: "Pause and ask above (files)",
  strong_markers_only: "Only explicit markers (v2, copy, final)",
  min_bytes: "Ignore files smaller than (bytes)",
  keeper_must_be_outside_downloads: "Kept copy must be outside Downloads",
  copies_in_downloads_only: "Only trash copies inside Downloads",
};

function days(secs: number): string {
  const d = Math.ceil(secs / 86400);
  return d <= 1 ? "less than a day" : `${d} days`;
}

export async function automateView(main: HTMLElement, ctx: AppCtx) {
  const body = h("div");
  main.append(h("div", { class: "page-head" }, h("h1", null, "Automate")), spinner());

  async function load() {
    const list = await rpc<List>("automate.list");
    clear(main);
    clear(body);
    const armed = list.rules.filter((r) => r.rule.state === "armed").length;
    const banner =
      list.mode !== "automate"
        ? h("div", { class: "problem", style: { marginBottom: "12px" } }, `Mode is ${list.mode}: rules keep previewing but nothing runs until you switch to Automate in Settings.`, " ", button("Open Settings", () => ctx.go("settings"), { small: true, kind: "ghost" }))
        : armed === 0
          ? h("p", { class: "muted" }, "Automate mode is on, but no rule is armed yet. Nothing runs unattended until you arm one.")
          : null;

    main.append(h("div", null,
      h("div", { class: "page-head" }, h("h1", null, "Automate"), h("span", { class: "sub" }, `tier-0 rules · each shows its work for ${list.preview_days} days before it can be armed · every run is undoable`), h("span", { class: "spacer" }), button("Add a rule…", addRule, { kind: "primary" })),
      banner,
      body,
    ));

    if (list.rules.length === 0) {
      body.append(h("div", { class: "empty" }, "No rules. Add one: it starts in preview and shows what it would have done before it may run."));
      return;
    }

    for (const r of list.rules) body.append(ruleCard(r, list));
  }

  function ruleCard(l: Listed, list: List): HTMLElement {
    const r = l.rule;
    const w = l.would_have;
    const stateP = r.state === "armed" ? pill("armed", "tier1") : r.state === "paused" ? pill("paused", "tier3") : pill("preview");
    const fileList = w.files.length
      ? h("details", null, h("summary", { class: "muted" }, `${num(w.files.length)} file${w.files.length === 1 ? "" : "s"} it would have touched`), h("ul", { class: "mono", style: { fontSize: "12px", maxHeight: "180px", overflow: "auto" } }, w.files.slice(0, 300).map((f) => h("li", { title: f }, shortPath(f))), w.files.length > 300 ? h("li", { class: "muted" }, `… and ${w.files.length - 300} more`) : null))
      : h("span", { class: "muted" }, "nothing so far");

    const actions = h("div", { class: "row" },
      button("Preview now", () => preview(r), { small: true }),
      r.state !== "armed"
        ? button("Arm", async () => {
            if (!w.armable) {
              toast(w.armable_reason, "error");
              return;
            }
            if (!window.confirm(`Arm rule #${r.rule_id}? While the mode is Automate it will run on the agent's schedule (≤ ${r.params.max_items_per_run} files per run). Every run appears in History and can be undone.`)) return;
            try {
              await rpc("automate.arm", { id: r.rule_id });
              toast("Armed", "ok");
              await load();
            } catch (e) {
              toast(errorText(e), "error");
            }
          }, { kind: "primary", small: true, title: w.armable ? "" : w.armable_reason })
        : button("Pause", async () => {
            await rpc("automate.pause", { id: r.rule_id, reason: "paused by user" });
            toast("Paused");
            await load();
          }, { small: true }),
      button("Remove", async () => {
        if (!window.confirm("Remove this rule and its run log? No file is touched.")) return;
        await rpc("automate.remove", { id: r.rule_id });
        await load();
      }, { small: true, kind: "ghost" }),
    );

    const armNote = r.state === "armed"
      ? h("span", { class: "muted" }, `armed ${ago(r.armed_ts)}${list.mode === "automate" ? "" : " — waiting for Automate mode"}`)
      : w.armable
        ? h("span", { class: "muted" }, "ready to arm")
        : h("span", { class: "muted" }, w.armable_in_secs > 0 ? `can be armed in ${days(w.armable_in_secs)}` : w.armable_reason);

    return h("div", { class: "card", style: { marginBottom: "12px" } },
      h("div", { class: "row", style: { alignItems: "baseline" } }, h("h3", { style: { margin: "0" } }, KIND_LABEL[r.kind] ?? r.kind), stateP, pill("tier 0", "tier0"), h("span", { class: "spacer" }), armNote),
      h("p", { style: { marginTop: "6px" } }, l.describe),
      r.paused_reason ? h("div", { class: "problem" }, `Paused: ${r.paused_reason}`) : null,
      h("div", { class: "kv", style: { marginTop: "8px" } },
        h("span", { class: "k" }, `Last ${list.preview_days} days`), h("span", null, `would have moved ${num(w.files.length)} file${w.files.length === 1 ? "" : "s"} (${bytes(w.bytes)}) across ${w.dry_runs} dry run${w.dry_runs === 1 ? "" : "s"}${w.real_runs ? `, ${w.real_runs} real run${w.real_runs === 1 ? "" : "s"}` : ""}`),
        h("span", { class: "k" }, "Files"), fileList,
        h("span", { class: "k" }, "Last evaluated"), h("span", null, w.last_eval_ts ? ago(w.last_eval_ts) : "not yet — the agent evaluates rules on every tick"),
        w.last_problems.length ? h("span", { class: "k" }, "Problems") : null, w.last_problems.length ? h("div", { class: "problem" }, w.last_problems.map((p) => h("div", null, p))) : null,
        h("span", { class: "k" }, "Settings"), h("span", { class: "mono muted", style: { fontSize: "12px" } }, Object.entries(r.params).map(([k, v]) => `${k}=${String(v)}`).join("  ")),
      ),
      h("div", { style: { marginTop: "10px" } }, actions),
    );
  }

  async function preview(r: Rule) {
    const content = h("div", null, spinner());
    modal(`${KIND_LABEL[r.kind] ?? r.kind} — right now`, content);
    try {
      const p = await rpc<Preview>("automate.preview", { id: r.rule_id });
      clear(content);
      content.append(h("div", null,
        h("p", null, `${num(p.now.steps)} file${p.now.steps === 1 ? "" : "s"} (${bytes(p.now.bytes)})${p.now.capped ? ` — ${num(p.now.candidates)} qualify, capped per run` : ""}. Nothing is moved by this preview.`),
        p.now.problems.length ? h("div", { class: "problem" }, p.now.problems.map((x) => h("div", null, x))) : null,
        p.keeps.length ? h("div", { class: "muted", style: { fontSize: "12px" } }, `${p.keeps.length} kept in place`) : null,
        p.now.diff.trim() ? renderDiff(p.now.diff) : h("div", { class: "empty" }, "Nothing qualifies right now."),
      ));
    } catch (e) {
      clear(content);
      content.append(h("div", { class: "problem" }, errorText(e)));
    }
  }

  async function addRule() {
    const kinds = await rpc<KindInfo[]>("automate.kinds");
    let chosen = kinds[0];
    let params: Record<string, unknown> = { ...chosen.defaults };
    const form = h("div");
    function render() {
      clear(form);
      const sel = h("select", { onChange: (e: Event) => { chosen = kinds.find((k) => k.kind === (e.target as HTMLSelectElement).value) ?? kinds[0]; params = { ...chosen.defaults }; render(); } },
        kinds.map((k) => h("option", { value: k.kind, selected: k.kind === chosen.kind }, KIND_LABEL[k.kind] ?? k.kind)));
      const fields = h("div", { class: "kv", style: { marginTop: "10px" } }, h("span", { class: "k" }, "Rule"), sel, h("span", null), h("span", { class: "muted" }, chosen.describe));
      for (const [k, v] of Object.entries(params)) {
        const label = PARAM_LABEL[k] ?? k;
        if (typeof v === "boolean") {
          fields.append(h("span", { class: "k" }, label), h("input", { type: "checkbox", checked: v, onChange: (e: Event) => (params[k] = (e.target as HTMLInputElement).checked) }));
        } else {
          fields.append(h("span", { class: "k" }, label), h("input", { type: "number", value: String(v), onInput: (e: Event) => (params[k] = Number((e.target as HTMLInputElement).value)) }));
        }
      }
      form.append(fields);
    }
    render();
    const m = modal("Add a rule", form, [
      button("Cancel", () => m.close()),
      button("Create in preview", async () => {
        try {
          const r = await rpc<{ rule: Rule; evaluation: { steps: number; bytes: number } }>("automate.add", { kind: chosen.kind, params });
          m.close();
          toast(`Rule #${r.rule.rule_id} created — previewing. Right now it would touch ${num(r.evaluation.steps)} file${r.evaluation.steps === 1 ? "" : "s"}.`, "ok");
          await load();
        } catch (e) {
          toast(errorText(e), "error");
        }
      }, { kind: "primary" }),
    ]);
  }

  try {
    await load();
  } catch (e) {
    clear(main);
    main.append(h("div", { class: "page-head" }, h("h1", null, "Automate")), h("div", { class: "problem" }, errorText(e)));
  }
}
