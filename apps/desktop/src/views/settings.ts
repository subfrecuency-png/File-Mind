import type { AppCtx } from "../main";
import { agentInfo, agentStart, agentStop, isTauri, pickFolder, rpc } from "../api";
import { ago, button, clear, date, errorText, h, num, pill, shortPath, spinner, toast } from "../ui";

interface AiConfig { adapter: "none" | "ollama" | "cloud"; ollama_url: string; ollama_model: string; cloud_model: string; cloud_key: string }
interface Audit { id: number; ts: number; adapter: string; purpose: string; bytes_sent: number; local: boolean; ok: boolean; latency_ms: number | null }
interface Embed { model: string; semantic: boolean; vectors: number; embedded: number; pending: number; model_installed: boolean }

export async function settingsView(main: HTMLElement, ctx: AppCtx) {
  main.append(h("div", { class: "page-head" }, h("h1", null, "Settings")), spinner());
  const [roots, mode, ai, audit, emb, agent] = await Promise.all([
    rpc<string[]>("roots.list"),
    rpc<string>("mode.get"),
    rpc<AiConfig>("ai.get"),
    rpc<Audit[]>("ai.audit", { limit: 40 }),
    rpc<Embed>("embed.status").catch(() => null),
    agentInfo(),
  ]);
  clear(main);

  // ---- mode ---------------------------------------------------------------
  const modeBox = h("div");
  let curMode = mode;
  function renderMode() {
    clear(modeBox);
    const opts: [string, string, string][] = [
      ["observe", "Observe", "Index, search and suggest. Never moves a file."],
      ["assist", "Assist", "Apply suggestions after you approve each plan. Everything undoable."],
      ["automate", "Automate", "Tier-0 rules may run unattended (Phase 9). Not yet available; behaves like Assist."],
    ];
    for (const [k, title, desc] of opts) {
      modeBox.append(h("div", { class: `mode-opt ${curMode === k ? "active" : ""}`, onClick: async () => {
        try {
          await rpc("mode.set", { mode: k });
          curMode = k;
          renderMode();
          await ctx.refreshNav();
          toast(`Mode: ${k}`, "ok");
        } catch (e) {
          toast(errorText(e), "error");
        }
      } }, h("span", null, curMode === k ? "●" : "○"), h("div", null, h("b", null, title), h("span", null, desc))));
    }
  }
  renderMode();

  // ---- roots --------------------------------------------------------------
  const rootsBox = h("div");
  function renderRoots(list: string[]) {
    clear(rootsBox);
    rootsBox.append(
      h("table", null, h("tbody", null, list.map((r) => h("tr", null, h("td", { class: "path", title: r }, shortPath(r)), h("td", { class: "right" }, button("Forget", async () => {
        if (!window.confirm(`Stop indexing ${shortPath(r)}? Files on disk are untouched; the index entries are removed.`)) return;
        try {
          await rpc("roots.remove", { path: r });
          renderRoots(await rpc<string[]>("roots.list"));
        } catch (e) {
          toast(errorText(e), "error");
        }
      }, { small: true, kind: "ghost" })))))),
      h("div", { style: { marginTop: "8px" } }, button("Add a folder…", async () => {
        const p = await pickFolder();
        if (!p) return;
        try {
          await rpc("roots.add", { path: p });
          toast("Added — scanning in the background", "ok");
          rpc("scan", { path: p }).catch(() => undefined);
          renderRoots(await rpc<string[]>("roots.list"));
        } catch (e) {
          toast(errorText(e), "error");
        }
      })),
    );
  }
  renderRoots(roots);

  // ---- AI -----------------------------------------------------------------
  const aiBox = h("div");
  let cfg = { ...ai };
  function renderAi() {
    clear(aiBox);
    const adapterSel = h("select", { onChange: (e: Event) => { cfg.adapter = (e.target as HTMLSelectElement).value as AiConfig["adapter"]; renderAi(); } },
      ["none", "ollama", "cloud"].map((a) => h("option", { value: a, selected: cfg.adapter === a }, a === "none" ? "None (search only)" : a === "ollama" ? "Ollama (on this Mac)" : "Anthropic API (cloud, opt-in)")));
    const fields = h("div", { class: "kv", style: { marginTop: "10px" } }, h("span", { class: "k" }, "Adapter"), adapterSel);
    if (cfg.adapter === "ollama") {
      fields.append(
        h("span", { class: "k" }, "Model"), h("input", { type: "text", value: cfg.ollama_model, onInput: (e: Event) => (cfg.ollama_model = (e.target as HTMLInputElement).value) }),
        h("span", { class: "k" }, "URL"), h("input", { type: "text", value: cfg.ollama_url, onInput: (e: Event) => (cfg.ollama_url = (e.target as HTMLInputElement).value) }),
      );
      if (/:cloud$|-cloud/.test(cfg.ollama_model)) fields.append(h("span", null), h("div", { class: "problem" }, "This is an Ollama cloud model: Ollama forwards each question to its hosted service, so the context leaves this machine. The audit log marks these calls CLOUD."));
    }
    if (cfg.adapter === "cloud") {
      fields.append(
        h("span", { class: "k" }, "Model"), h("input", { type: "text", value: cfg.cloud_model, onInput: (e: Event) => (cfg.cloud_model = (e.target as HTMLInputElement).value) }),
        h("span", { class: "k" }, "API key"), h("input", { type: "password", value: cfg.cloud_key, placeholder: "sk-ant-… (or set ANTHROPIC_API_KEY)", onInput: (e: Event) => (cfg.cloud_key = (e.target as HTMLInputElement).value) }),
        h("span", null), h("div", { class: "problem" }, "Each question sends file names, folders, dates and short excerpts — at most 2 KB — to Anthropic. Every call is listed in the audit log below."),
      );
    }
    aiBox.append(fields, h("div", { style: { marginTop: "10px" } }, button("Save", async () => {
      try {
        cfg = await rpc<AiConfig>("ai.set", cfg as unknown as Record<string, unknown>);
        toast("AI settings saved", "ok");
      } catch (e) {
        toast(errorText(e), "error");
      }
    }, { kind: "primary" })));
  }
  renderAi();

  const auditTable = audit.length
    ? h("table", null, h("thead", null, h("tr", null, h("th", null, "When"), h("th", null, "Adapter"), h("th", null, "Purpose"), h("th", { class: "right" }, "Sent"), h("th", null, "Where"), h("th", null, "Result"))),
        h("tbody", null, audit.map((a) => h("tr", null, h("td", { class: "muted" }, ago(a.ts)), h("td", null, a.adapter), h("td", null, a.purpose), h("td", { class: "right" }, `${num(a.bytes_sent)} B`), h("td", null, a.local ? pill("local") : pill("CLOUD", "tier2")), h("td", null, a.ok ? `ok${a.latency_ms ? ` · ${a.latency_ms} ms` : ""}` : pill("failed", "failed"))))))
    : h("div", { class: "empty" }, "Nothing has been sent to any adapter.");

  // ---- agent & model --------------------------------------------------------
  const agentBox = h("div", { class: "kv" },
    h("span", { class: "k" }, "Status"), h("span", null, agent.running ? "running" : agent.stale ? "running, but from an older build — stop and start it" : "not running (the app works on the database directly; live updates need the agent)"),
    h("span", { class: "k" }, "Binary"), h("span", { class: "path" }, agent.binary ? shortPath(agent.binary) : "not found — set FILEMIND_AGENT_BIN or run `filemind agent start`"),
    h("span", { class: "k" }, "Build"), h("span", { class: "mono" }, agent.build),
    h("span", null), h("div", { class: "row" },
      button(agent.running ? "Restart" : "Start agent", async () => {
        try {
          if (agent.running || agent.stale) {
            await agentStop();
            await new Promise((r) => setTimeout(r, 1500));
          }
          const r = await agentStart();
          toast(`Agent ${JSON.stringify(r)}`, "ok");
          ctx.go("settings");
        } catch (e) {
          toast(errorText(e), "error");
        }
      }, { kind: "primary", small: true }),
      agent.running || agent.stale ? button("Stop", async () => { await agentStop(); toast("Stop requested"); setTimeout(() => ctx.go("settings"), 1500); }, { small: true }) : null,
    ),
  );

  const modelBox = h("div", { class: "kv" },
    h("span", { class: "k" }, "Embedding model"), h("span", null, emb ? (emb.model_installed ? `${emb.model} installed` : "not installed — run `filemind model download` in a terminal (133 MB)") : "—"),
    h("span", { class: "k" }, "Vectors"), h("span", null, emb ? `${num(emb.vectors)}${emb.pending ? ` · ${num(emb.pending)} pending` : " · up to date"}` : "—"),
    h("span", null), emb && emb.pending ? button("Embed pending now", async () => {
      toast("Embedding for up to 5 minutes…");
      try {
        const r = await rpc<{ embedded: number; remaining: number }>("embed.run", { duty: 1, minutes: 5 });
        toast(`${num(r.embedded)} embedded, ${num(r.remaining)} remaining`, "ok");
      } catch (e) {
        toast(errorText(e), "error");
      }
    }, { small: true }) : h("span"),
  );

  main.append(
    h("div", { class: "page-head" }, h("h1", null, "Settings")),
    h(
      "div",
      { class: "grid cols-2" },
      h("div", { class: "card" }, h("h3", null, "Mode"), modeBox),
      h("div", { class: "card" }, h("h3", null, "Folders"), rootsBox),
      h("div", { class: "card" }, h("h3", null, "AI adapter for “Ask”"), aiBox),
      h("div", { class: "card" }, h("h3", null, "Background agent"), agentBox, h("h3", { style: { marginTop: "16px" } }, "Search index"), modelBox),
    ),
    h("div", { class: "card", style: { marginTop: "14px" } }, h("h3", null, "AI audit log"), h("p", { class: "muted", style: { fontSize: "12px", marginTop: "0" } }, "What was sent to which model. The text itself is never stored — only its size and a hash."), auditTable),
    !isTauri ? h("p", { class: "muted", style: { fontSize: "12px" } }, `Browser preview with mock data · ${date(Math.floor(Date.now() / 1000))}`) : h("span"),
  );
}
