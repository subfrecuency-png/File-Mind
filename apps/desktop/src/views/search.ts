import type { AppCtx } from "../main";
import { reveal, rpc, openPath } from "../api";
import { ago, bytes, button, clear, errorText, h, parentDir, pill, shortPath, spinner, toast } from "../ui";

interface Hit { file_id: string; path: string; name: string; size: number; mtime: number; category: string | null; sensitive: boolean; score: number; via: string[]; location?: string | null }
interface Note { note_id: number; subject_id: string; text: string; ts: number }
interface SearchResult { query: string; parsed: { text: string; notes: string[] }; semantic: boolean; hits: Hit[]; notes: Note[]; elapsed_ms: number }
interface Answer { answer: string; adapter: string; local: boolean; bytes_sent: number; hits: Hit[] }

let lastQuery = "";

export async function searchView(main: HTMLElement, ctx: AppCtx) {
  let mode = "hybrid";
  const input = h("input", { type: "search", class: "big", placeholder: "that offer sheet for the calcium supplier from last spring, pdf…", value: ctx.param ?? lastQuery, autofocus: true });
  const results = h("div");
  const seg = h("div", { class: "seg" });
  const askBtn = button("Ask", () => ask(), { kind: "primary" });
  const status = h("div", { class: "explain" });

  function renderSeg() {
    clear(seg);
    for (const m of ["hybrid", "lexical", "semantic"]) {
      seg.append(h("button", { class: m === mode ? "active" : "", onClick: () => { mode = m; renderSeg(); run(); } }, m === "lexical" ? "words" : m === "semantic" ? "meaning" : "both"));
    }
  }
  renderSeg();

  /** "archive:<id>#<rel>" → [id, rel]; null for a plain present file. */
  function inArchive(hit: Hit): [string, string] | null {
    if (!hit.location || !hit.location.startsWith("archive:")) return null;
    const rest = hit.location.slice("archive:".length);
    const at = rest.indexOf("#");
    return at < 0 ? null : [rest.slice(0, at), rest.slice(at + 1)];
  }

  async function restoreMember(hit: Hit) {
    const arc = inArchive(hit);
    if (!arc) return;
    try {
      await rpc("archive.restore", { id: arc[0], member: arc[1] });
      toast(`Restored ${hit.name} to its original place`, "ok");
      run();
    } catch (e) {
      toast(errorText(e), "error");
    }
  }

  function hitRow(hit: Hit, i: number) {
    const arc = inArchive(hit);
    return h(
      "div",
      { class: "hit" },
      h("div", { class: "n" }, String(i + 1)),
      h(
        "div",
        null,
        h("div", { class: "name" }, hit.name, hit.sensitive ? [" ", pill("sensitive", "sensitive")] : null, arc ? [" ", pill("in archive", "archive")] : null),
        h("div", { class: "path muted", title: hit.path }, shortPath(parentDir(hit.path))),
        h("div", { class: "meta" }, h("span", null, ago(hit.mtime)), h("span", null, bytes(hit.size)), hit.category ? h("span", null, hit.category) : null, h("span", { class: "via" }, hit.via.join(" · "))),
      ),
      arc
        ? h("div", { class: "row" }, button("Restore", () => restoreMember(hit), { small: true }), button("Note", () => addNote(hit.path), { small: true, kind: "ghost" }))
        : h("div", { class: "row" }, button("Reveal", () => reveal(hit.path), { small: true }), button("Open", () => openPath(hit.path), { small: true }), button("Note", () => addNote(hit.path), { small: true, kind: "ghost" })),
    );
  }

  async function addNote(path: string) {
    const text = window.prompt(`Remember something about ${shortPath(path)}:`);
    if (!text) return;
    try {
      await rpc("notes.add", { subject: path, text, kind: "file" });
      toast("Note saved — it is searchable now", "ok");
    } catch (e) {
      toast(errorText(e), "error");
    }
  }

  async function run() {
    const q = input.value.trim();
    lastQuery = q;
    if (!q) {
      clear(results);
      clear(status);
      return;
    }
    clear(results);
    results.append(spinner("Searching…"));
    try {
      const r = await rpc<SearchResult>("search", { query: q, limit: 30, mode });
      clear(results);
      clear(status);
      const bits: string[] = [];
      if (r.parsed.text) bits.push(`looking for “${r.parsed.text}”`);
      bits.push(...r.parsed.notes);
      status.append(bits.join("  ·  "), h("span", { class: "muted" }, `  (${r.elapsed_ms} ms${r.semantic ? "" : ", words only — embedding model not installed"})`));
      if (r.notes.length) {
        results.append(h("div", { style: { marginBottom: "10px" } }, r.notes.map((n) => h("div", { class: "note-card" }, h("b", null, "note"), h("span", null, n.text), h("span", { class: "muted path", title: n.subject_id }, shortPath(n.subject_id))))));
      }
      if (r.hits.length === 0) results.append(h("div", { class: "empty" }, "Nothing matched. Try fewer words, or the “meaning” mode."));
      results.append(...r.hits.map(hitRow));
    } catch (e) {
      clear(results);
      results.append(h("div", { class: "problem" }, errorText(e)));
    }
  }

  async function ask() {
    const q = input.value.trim();
    if (!q) return;
    clear(results);
    results.append(spinner("Asking the configured AI adapter…"));
    try {
      const a = await rpc<Answer>("ask", { question: q });
      clear(results);
      results.append(
        h("div", { class: "answer" }, a.answer, h("div", { class: "foot" }, `${a.adapter} · ${a.bytes_sent} bytes of file names, folders, dates and short excerpts ${a.local ? "stayed on this machine" : "were sent to the cloud"} · see Settings → AI audit`)),
        ...a.hits.map(hitRow),
      );
    } catch (e) {
      clear(results);
      results.append(h("div", { class: "problem" }, errorText(e)), h("div", { style: { marginTop: "10px" } }, button("Configure an AI adapter", () => ctx.go("settings"))));
    }
  }

  let timer: number | undefined;
  input.addEventListener("input", () => {
    window.clearTimeout(timer);
    timer = window.setTimeout(run, 250);
  });
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") {
      window.clearTimeout(timer);
      if (e.metaKey || e.ctrlKey) ask();
      else run();
    }
  });

  main.append(
    h("div", { class: "page-head" }, h("h1", null, "Search"), h("span", { class: "sub" }, "plain language works: dates, kinds, folders, projects"), h("span", { class: "spacer" }), seg),
    h("div", { class: "row", style: { marginBottom: "6px" } }, h("div", { style: { flex: "1" } }, input), askBtn),
    h("div", { class: "muted", style: { fontSize: "12px", marginBottom: "10px" } }, "Enter searches · ⌘Enter asks the AI a question about the results"),
    status,
    h("div", { class: "card", style: { padding: "0" } }, results),
  );
  if (input.value) run();
  setTimeout(() => input.focus(), 0);
}
