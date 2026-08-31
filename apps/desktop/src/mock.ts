// Browser-only stand-in for the agent, shaped exactly like the real RPC
// replies (see crates/agent/src/rpc.rs). Enough state to click through
// every screen: suggestions can be planned/applied/dismissed, transactions
// undone, notes added, mode and adapter switched.

const HOME = "/Users/ryan";
const now = Math.floor(Date.now() / 1000);
const day = 86400;

let mode = "assist";
let ai = { adapter: "ollama", ollama_url: "http://127.0.0.1:11434", ollama_model: "nemotron-3-super:cloud", cloud_model: "claude-opus-5", cloud_key: "" };
let roots = [`${HOME}/Downloads`, `${HOME}/Documents`];
const notes: Record<string, unknown>[] = [
  { note_id: 1, subject_type: "file", subject_id: `${HOME}/Downloads/OFFER SHEET Calcium.pdf`, text: "the one we accepted, signed in May", source: "user", ts: now - 2 * day },
];
let nextNote = 2;

const suggestions: Record<string, unknown>[] = [
  { id: 3913, kind: "trash_duplicates", risk_tier: 2, est_bytes: 3_200_000, state: "proposed",
    subject: { keep: `${HOME}/Documents/Offers/OFFER SHEET Calcium.pdf`, trash: [`${HOME}/Downloads/OFFER SHEET Calcium.pdf`, `${HOME}/Downloads/OFFER SHEET Calcium copy.pdf`] },
    rationale: "2 identical copies of OFFER SHEET Calcium.pdf — keep the one in Documents/Offers and move the rest to Trash (reversible)." },
  { id: 1234, kind: "trash_duplicates", risk_tier: 2, est_bytes: 1_800_000, state: "proposed",
    subject: { keep: `${HOME}/Downloads/UENUKE_ProjectFiles_v2/LS_Plane_Title6_v008_0122.jpeg`, trash: [`${HOME}/Downloads/ProjectFiles_StartHere_v2/Examples/LS_Plane_Title6/LS_Plane_Title6_v008_0122.jpeg`] },
    rationale: "1 identical copy of LS_Plane_Title6_v008_0122.jpeg — keep the one in UENUKE_ProjectFiles_v2 and move the rest to Trash (reversible)." },
  { id: 2201, kind: "collapse_versions", risk_tier: 1, est_bytes: 0, state: "proposed",
    subject: { keep: `${HOME}/Documents/Pitch/creditos deck v7.key`, older: [`${HOME}/Documents/Pitch/creditos deck v5.key`, `${HOME}/Documents/Pitch/creditos deck v6.key`] },
    rationale: "3 versions of creditos deck — v7 is newest; tuck v5 and v6 into a 'creditos deck versions' folder beside it." },
  { id: 4102, kind: "compress_cold_text", risk_tier: 1, est_bytes: 1_370_000_000, state: "proposed",
    subject: { root: `${HOME}/Downloads`, bucket: "code", ratio: 0.28, method: "apfs", files: [`${HOME}/Downloads/creditos-v1/src/ledger.ts`, `${HOME}/Downloads/creditos-v1/src/api.ts`, `${HOME}/Downloads/creditos-v1/package-lock.json`], bytes: 1_900_000_000, total_files: 38_000, total_bytes: 1_900_000_000 },
    rationale: "500 code files (412.0 MB) untouched for 30+ days in Downloads could take about 296.6 MB less space with APFS transparent compression. They stay exactly the same to every app; reversible in place. (37,500 more qualify; they come in the next batch.)" },
  { id: 4201, kind: "archive_cold_project", risk_tier: 1, est_bytes: 730_000_000, state: "proposed",
    subject: { project_id: 17, name: "SaberBattle 5", folder: `${HOME}/Downloads/SaberBattle 5`, files: 6_362, bytes: 5_700_000_000, saving: 730_000_000 },
    rationale: "SaberBattle 5 has not been touched in 7 months — pack it (5.7 GB in 6,362 files) into a verified compressed archive, reclaiming about 730.0 MB. Search still finds every file inside; restore is one command. The original goes to Trash only after every file in the archive is decoded and checked." },
  { id: 3859, kind: "stale_downloads", risk_tier: 1, est_bytes: 34_000_000_000, state: "proposed",
    subject: { root: `${HOME}/Downloads`, older_than_days: 90 },
    rationale: "1,204 items in Downloads untouched for over 90 days (34.0 GB) — archive them into ~/FileMind Archive/Downloads by month." },
];

const txns: Record<string, unknown>[] = [
  { txn_id: "txn_20260829T231945_20b8", state: "undone", created_ts: now - day, executed_ts: now - day + 2, rationale: "1 identical copy of LS_Plane_Title6_v008_0122.jpeg …", steps: 1, done: 1 },
];

const projects = [
  { project_id: 12, key: "folder:creditos", kind: "folder", name: "Creditos", suggested_name: "creditos", root_path: `${HOME}/Projects/creditos`, file_count: 412, bytes: 88_000_000, start_ts: now - 120 * day, end_ts: now - day, activity_score: 0.91, status: "active" },
  { project_id: 4, key: "folder:filemind", kind: "folder", name: null, suggested_name: "FileMind", root_path: `${HOME}/Documents/filemind`, file_count: 260, bytes: 12_000_000, start_ts: now - 30 * day, end_ts: now, activity_score: 0.98, status: "active" },
  { project_id: 31, key: "topic:offer-sheet", kind: "topic", name: null, suggested_name: "Offer Sheet", root_path: null, file_count: 18, bytes: 61_000_000, start_ts: now - 200 * day, end_ts: now - 40 * day, activity_score: 0.22, status: "active" },
  { project_id: 7, key: "folder:lightsaber", kind: "folder", name: "Lightsaber shots", suggested_name: "LS_Plane", root_path: `${HOME}/Downloads/UENUKE_ProjectFiles_v2`, file_count: 2080, bytes: 9_800_000_000, start_ts: now - 300 * day, end_ts: now - 150 * day, activity_score: 0.05, status: "active" },
];

const files = [
  { file_id: "1:1", path: `${HOME}/Downloads/OFFER SHEET Calcium.pdf`, name: "OFFER SHEET Calcium.pdf", ext: "pdf", size: 1_600_000, mtime: now - 140 * day, category: "contract", sensitive: false, status: "present" },
  { file_id: "1:2", path: `${HOME}/Documents/Offers/OFFER SHEET Calcium.pdf`, name: "OFFER SHEET Calcium.pdf", ext: "pdf", size: 1_600_000, mtime: now - 140 * day, category: "contract", sensitive: false, status: "present" },
  { file_id: "1:3", path: `${HOME}/Documents/Finance/invoice_acme_2026-02.pdf`, name: "invoice_acme_2026-02.pdf", ext: "pdf", size: 120_000, mtime: now - 200 * day, category: "invoice", sensitive: false, status: "present" },
  { file_id: "1:4", path: `${HOME}/Documents/Legal/lease apartment 2024.pdf`, name: "lease apartment 2024.pdf", ext: "pdf", size: 400_000, mtime: now - 700 * day, category: "contract", sensitive: false, status: "present" },
  { file_id: "1:5", path: `${HOME}/Downloads/secrets.env`, name: "secrets.env", ext: "env", size: 900, mtime: now - 3 * day, category: "code", sensitive: true, status: "present" },
  { file_id: "1:6", path: `${HOME}/Pictures/Screenshots/Screenshot 2026-08-28 at 10.14.32.png`, name: "Screenshot 2026-08-28 at 10.14.32.png", ext: "png", size: 2_300_000, mtime: now - 2 * day, category: "screenshot", sensitive: false, status: "present" },
  { file_id: "1:7", path: `${HOME}/Downloads/SaberBattle 5/Assets/Scripts/SaberController.cs`, name: "SaberController.cs", ext: "cs", size: 41_000, mtime: now - 220 * day, category: "code", sensitive: false, status: "archived", location: "archive:arc_mock#Assets/Scripts/SaberController.cs" },
];

function sleep(ms: number) {
  return new Promise((r) => setTimeout(r, ms));
}

interface MockJob { id: number; kind: string; state: string; label: string; done: number; total: number; started_ms: number; elapsed_ms: number; result: unknown; error: string | null }
const jobs: MockJob[] = [];
let shrinkEstimate: Record<string, unknown> | null = null;
function mockShrinkEstimate() {
  const now = Math.floor(Date.now() / 1000);
  return {
    computed_ts: now, elapsed_ms: 4200, files_seen: 188_177, bytes_seen: 54_700_000_000, sampled_files: 310, sampled_bytes: 58_000_000,
    saving_bytes: 1_600_000_000 + 3_100_000_000 + 12_400_000_000,
    tiers: [
      { tier: 1, kind: "apfs", label: "APFS transparent compression", note: "files stay ordinary files; only the on-disk footprint shrinks; reversible in place", measured: true, candidate_files: 41_200, candidate_bytes: 2_300_000_000, ratio: 0.30, saving_bytes: 1_600_000_000,
        buckets: [{ name: "code", files: 38_000, bytes: 1_900_000_000, ratio: 0.28, saving_bytes: 1_370_000_000, sampled_files: 48, sampled_bytes: 9_000_000 }, { name: "document", files: 3_200, bytes: 400_000_000, ratio: 0.42, saving_bytes: 230_000_000, sampled_files: 48, sampled_bytes: 9_000_000 }] },
      { tier: 2, kind: "media_lossless", label: "lossless JPEG XL / PNG recompression", note: "typical ratios, not measured yet", measured: false, candidate_files: 58_900, candidate_bytes: 14_000_000_000, ratio: 0.78, saving_bytes: 3_100_000_000,
        buckets: [{ name: "jpeg", files: 55_000, bytes: 13_000_000_000, ratio: 0.78, saving_bytes: 2_860_000_000, sampled_files: 0, sampled_bytes: 0 }, { name: "png", files: 3_900, bytes: 1_000_000_000, ratio: 0.85, saving_bytes: 150_000_000, sampled_files: 0, sampled_bytes: 0 }] },
      { tier: 3, kind: "cold_archive", label: "cold-project archives", note: "projects untouched for 180 days packed with zstd -19", measured: true, candidate_files: 12_400, candidate_bytes: 34_000_000_000, ratio: 0.64, saving_bytes: 12_400_000_000,
        buckets: [{ name: "media", files: 900, bytes: 20_000_000_000, ratio: 0.99, saving_bytes: 200_000_000, sampled_files: 48, sampled_bytes: 9_000_000 }, { name: "code", files: 11_000, bytes: 9_000_000_000, ratio: 0.2, saving_bytes: 7_200_000_000, sampled_files: 48, sampled_bytes: 9_000_000 }, { name: "data", files: 500, bytes: 5_000_000_000, ratio: 0.0, saving_bytes: 5_000_000_000, sampled_files: 48, sampled_bytes: 9_000_000 }],
        projects: [{ project_id: 7, name: "Lightsaber shots", root_path: `${HOME}/Downloads/lightsaber-shots`, end_ts: now - 200 * 86_400, files: 12_400, bytes: 34_000_000_000, saving_bytes: 12_400_000_000 }] },
    ],
  };
}

function startMockJob(kind: string): MockJob {
  const total = kind === "model.download" ? 133_000_000 : kind === "analyze" ? 0 : kind === "shrink.estimate" ? 310 : 188_177;
  const j: MockJob = { id: jobs.length + 1, kind, state: "running", label: kind === "scan" ? `scanning ${HOME}/Downloads` : kind === "model.download" ? "model.onnx" : kind, done: 0, total, started_ms: Date.now(), elapsed_ms: 0, result: null, error: null };
  jobs.push(j);
  const t0 = Date.now();
  const tick = setInterval(() => {
    j.elapsed_ms = Date.now() - t0;
    j.done = Math.min(total, Math.round((j.elapsed_ms / 3000) * total));
    if (j.elapsed_ms >= 3000) {
      clearInterval(tick);
      j.state = "done";
      j.done = total;
      if (kind === "shrink.estimate") shrinkEstimate = mockShrinkEstimate();
      j.result = kind === "scan" ? [{ path: `${HOME}/Downloads`, files: 60_211, elapsed_ms: 2900 }] : kind === "analyze" ? { suggestions: suggestions.length, duplicate_groups: 2630 } : kind === "embed" ? { embedded: 4_120, remaining: 0 } : kind === "shrink.estimate" ? { estimate: shrinkEstimate, cached: false } : { installed: true };
    }
  }, 200);
  return j;
}

const RULE_KINDS = [
  { kind: "archive_stale_downloads", defaults: { max_items_per_run: 50, older_than_days: 90, pause_above: 300 }, describe: "archive loose Downloads untouched for 90 days into ~/FileMind Archive (≤ 50 per run)" },
  { kind: "collapse_versions", defaults: { max_items_per_run: 50, older_than_days: 14, pause_above: 300, strong_markers_only: true }, describe: "tuck older versions with explicit markers untouched for 14 days into a versions folder (≤ 50 per run)" },
  { kind: "trash_exact_duplicates", defaults: { copies_in_downloads_only: true, keeper_must_be_outside_downloads: true, max_items_per_run: 50, min_bytes: 1048576, older_than_days: 30, pause_above: 300 }, describe: "trash exact copies ≥ 1.0 MB untouched for 30 days that sit in Downloads, keeper outside Downloads (≤ 50 per run)" },
];
interface MockRule { rule_id: number; kind: string; params: Record<string, unknown>; tier: number; state: string; created_ts: number; armed_ts: number | null; paused_ts: number | null; paused_reason: string | null }
const rules: MockRule[] = [
  { rule_id: 1, kind: "archive_stale_downloads", params: { max_items_per_run: 50, older_than_days: 90, pause_above: 300 }, tier: 0, state: "preview", created_ts: now - 3 * day, armed_ts: null, paused_ts: null, paused_reason: null },
  { rule_id: 2, kind: "trash_exact_duplicates", params: { copies_in_downloads_only: true, keeper_must_be_outside_downloads: true, max_items_per_run: 50, min_bytes: 1048576, older_than_days: 30, pause_above: 300 }, tier: 0, state: "armed", created_ts: now - 12 * day, armed_ts: now - 2 * day, paused_ts: null, paused_reason: null },
  { rule_id: 3, kind: "collapse_versions", params: { max_items_per_run: 50, older_than_days: 14, pause_above: 300, strong_markers_only: true }, tier: 0, state: "paused", created_ts: now - 20 * day, armed_ts: now - 9 * day, paused_ts: now - day, paused_reason: "step 2: /Users/ryan/Documents/Pitch/creditos deck v5.key changed since it was planned" },
];
function wouldHave(r: MockRule) {
  const files = r.kind === "archive_stale_downloads" ? [`${HOME}/Downloads/old-installer.dmg`, `${HOME}/Downloads/receipt (3).pdf`, `${HOME}/Downloads/IMG_2231.HEIC`] : r.kind === "trash_exact_duplicates" ? [`${HOME}/Downloads/OFFER SHEET Calcium.pdf`] : [`${HOME}/Documents/Pitch/creditos deck v5.key`, `${HOME}/Documents/Pitch/creditos deck v6.key`];
  const armable = now - r.created_ts >= 7 * day;
  return { rule_id: r.rule_id, since_ts: now - 7 * day, dry_runs: Math.min(48, Math.floor((now - r.created_ts) / 1800)), real_runs: r.state === "armed" ? 2 : 0, files, bytes: 41_000_000, last_eval_ts: now - 600, last_problems: r.paused_reason ? [r.paused_reason] : [], armable, armable_in_secs: armable ? 0 : r.created_ts + 7 * day - now, armable_reason: armable ? "" : "previewing: a rule can be armed 7 days after it was created" };
}

export async function mockRpc(method: string, p: Record<string, unknown>): Promise<unknown> {
  await sleep(60);
  switch (method) {
    case "telemetry.get":
      return { enabled: false, endpoint: "", install_id: null, last_sent_day: null, fields: ["schema", "install_id"], preview: { schema: 1, install_id: "", day: "2026-08-29", version: "0.2.0", os: "macos", arch: "aarch64", health_bucket: "70-79", files_bucket: "100k-250k", suggestions_applied: 1, suggestions_undone: 0, rules_armed: 0, rule_runs: 0, sessions: 3, crash_free_sessions: 3, undo_failures: 0, adapter: "ollama" } };
    case "telemetry.set":
      return { enabled: p.enabled };
    case "crash.list":
      return [{ name: "20260829T210000-agent.txt", bytes: 2400, ts: now - 3600, component: "agent", message: "index out of bounds: the len is 3 but the index is 7" }];
    case "crash.read":
      return { name: p.name, text: "FileMind crash report\ncomponent: agent\nversion: 0.2.0\nmessage: index out of bounds\n\nbacktrace:\n   0: filemind_agent::watcher::run\n" };
    case "crash.settle":
      return { settled: true };
    case "automate.kinds":
      return RULE_KINDS;
    case "automate.list":
      return { mode, preview_days: 7, rules: rules.map((r) => ({ rule: r, describe: RULE_KINDS.find((k) => k.kind === r.kind)?.describe ?? r.kind, would_have: wouldHave(r) })) };
    case "automate.add": {
      const kind = RULE_KINDS.find((k) => k.kind === p.kind) ?? RULE_KINDS[0];
      const r: MockRule = { rule_id: rules.length + 1, kind: kind.kind, params: { ...kind.defaults, ...(p.params as Record<string, unknown>) }, tier: 0, state: "preview", created_ts: now, armed_ts: null, paused_ts: null, paused_reason: null };
      rules.push(r);
      return { rule: r, evaluation: { steps: 3, bytes: 41_000_000, candidates: 3, capped: false, problems: [], diff: "" } };
    }
    case "automate.preview": {
      const r = rules.find((x) => x.rule_id === p.id)!;
      const w = wouldHave(r);
      const diff = w.files.map((f, i) => (r.kind === "trash_exact_duplicates" ? `  ${i}  TRASH  ${f}` : `  ${i}  MOVE   ${f}\n       →      ${HOME}/FileMind Archive/Downloads/2026/2026-03/${f.split("/").pop()}`)).join("\n");
      return { rule: r, now: { candidates: w.files.length, candidate_bytes: w.bytes, steps: w.files.length, bytes: w.bytes, capped: false, problems: w.last_problems, diff }, keeps: r.kind === "trash_exact_duplicates" ? [`${HOME}/Documents/Offers/OFFER SHEET Calcium.pdf`] : [], would_have: w };
    }
    case "automate.arm": {
      const r = rules.find((x) => x.rule_id === p.id)!;
      if (now - r.created_ts < 7 * day) throw new Error("cannot arm: previewing");
      r.state = "armed";
      r.armed_ts = now;
      r.paused_reason = null;
      return r;
    }
    case "automate.pause": {
      const r = rules.find((x) => x.rule_id === p.id)!;
      r.state = "paused";
      r.paused_reason = String(p.reason);
      return { paused: true };
    }
    case "automate.remove": {
      const i = rules.findIndex((x) => x.rule_id === p.id);
      if (i >= 0) rules.splice(i, 1);
      return { removed: i >= 0 };
    }
    case "shrink.estimate":
      if (p.cached_only && !shrinkEstimate) return { estimate: null, cached: true };
      if (!shrinkEstimate || p.refresh) {
        await sleep(600);
        shrinkEstimate = mockShrinkEstimate();
      }
      return { estimate: shrinkEstimate, cached: true };
    case "jobs.start":
      return startMockJob(String(p.kind));
    case "jobs.list":
      return jobs;
    case "jobs.status":
      return jobs.find((j) => j.id === p.id) ?? jobs[jobs.length - 1] ?? null;
    case "ping":
      return { pong: true, build: "mock" };
    case "status":
      return {
        platform: "macos", database: `${HOME}/Library/Application Support/FileMind/filemind.db`, mode,
        uptime_s: 5321, files: 188_177, dirs: 21_004, missing: 312, hashed: 187_900, bytes: 54_700_000_000, events: 206_442, transactions: txns.length,
        roots: roots.map((r) => ({ path: r, last_scan: now - 600 })),
        watcher: { roots: roots.length, raw_events: 1_204, changes_applied: 388, last_change_unix: now - 90 },
      };
    case "roots.list":
      return roots;
    case "roots.add":
      roots = [...roots, String(p.path)];
      return { root_id: roots.length, path: p.path };
    case "roots.remove":
      roots = roots.filter((r) => r !== p.path);
      return { removed: 1 };
    case "health":
      return {
        health: { score: 72, components: [
          { name: "duplicates", ratio: 0.31, weight: 30, penalty: 9.3, detail: "37.6 GB in 2,630 duplicate groups" },
          { name: "stale_downloads", ratio: 0.62, weight: 25, penalty: 15.5, detail: "1,204 items untouched > 90 days" },
          { name: "versions", ratio: 0.08, weight: 15, penalty: 1.2, detail: "116 version chains" },
          { name: "unclassified", ratio: 0.01, weight: 10, penalty: 0.1, detail: "1,900 files pending" },
          { name: "sensitive_exposed", ratio: 0.05, weight: 20, penalty: 1.0, detail: "91 sensitive files, 3 in Downloads" },
        ] },
        roots: roots.map((r, i) => ({ path: r, score: 72 - i * 9 })),
        history: Array.from({ length: 14 }, (_, i) => [now - (13 - i) * day, 58 + Math.round(i * 1.1)]),
      };
    case "categories":
      return { categories: [
        { category: "photo", files: 60_213, bytes: 21_000_000_000 }, { category: "code", files: 51_870, bytes: 1_200_000_000 },
        { category: "document", files: 19_204, bytes: 4_100_000_000 }, { category: "media", files: 4_100, bytes: 22_000_000_000 },
        { category: "design", files: 3_310, bytes: 5_600_000_000 }, { category: "invoice", files: 412, bytes: 90_000_000 },
        { category: "contract", files: 96, bytes: 40_000_000 }, { category: "other", files: 48_972, bytes: 700_000_000 },
      ], sensitive: 91, pending: 1_900, rules: [{ id: 3, rule: "PathPrefix(~/Documents/Invoices) → invoice" }] };
    case "embed.status":
      return { model: "bge-small-en-v1.5", semantic: true, vectors: 23_010, embedded: 23_009, pending: 0, model_installed: true };
    case "projects.list":
      return projects;
    case "projects.show": {
      const pr = projects.find((x) => x.project_id === p.id);
      return { project: pr, files: files.slice(0, 4).map((f) => ({ path: f.path, mtime: f.mtime, size: f.size })),
        categories: [{ category: "document", files: 120 }, { category: "code", files: 260 }, { category: "design", files: 32 }] };
    }
    case "projects.rename": {
      const pr = projects.find((x) => x.project_id === p.id);
      if (pr) pr.name = String(p.name);
      return pr;
    }
    case "suggest.list": {
      const items = suggestions.filter((s) => s.state === (p.state ?? "proposed"));
      const bytes = suggestions.filter((s) => s.state === "proposed").reduce((a, s) => a + (s.est_bytes as number), 0);
      return { proposed: suggestions.filter((s) => s.state === "proposed").length, est_bytes: bytes, items };
    }
    case "suggest.dismiss": {
      const s = suggestions.find((x) => x.id === p.id);
      if (s) s.state = "dismissed";
      return { ok: !!s };
    }
    case "suggest.plan": {
      const s = suggestions.find((x) => x.id === p.id);
      if (!s) throw new Error(`no proposed suggestion #${p.id}`);
      const sub = s.subject as Record<string, unknown>;
      let diff = "";
      let steps = 0;
      if (s.kind === "trash_duplicates") {
        diff += `     KEEP   ${sub.keep}\n`;
        (sub.trash as string[]).forEach((t, i) => { diff += `${String(i).padStart(3)}  TRASH  ${t}\n`; steps++; });
      } else if (s.kind === "compress_cold_text") {
        (sub.files as string[]).forEach((f, i) => { diff += `${String(i).padStart(3)}  SHRINK ${f}\n       apfs   ${[1.2, 0.8, 2.1][i] ?? 1} MB → ~${[340, 220, 590][i] ?? 300} KB on disk\n`; steps++; });
      } else if (s.kind === "archive_cold_project") {
        diff += `  0  TRASH  ${sub.folder}\n       (after packing 6,362 files into ${HOME}/FileMind Archive/Projects/SaberBattle 5 (arc_mock).fmpack and verifying every one)\n`;
        steps = 1;
      } else if (s.kind === "collapse_versions") {
        diff += `     KEEP   ${sub.keep}\n`;
        (sub.older as string[]).forEach((t, i) => { diff += `${String(i).padStart(3)}  MOVE   ${t}\n       →      ${HOME}/Documents/Pitch/creditos deck versions/${t.split("/").pop()}\n`; steps++; });
      } else {
        for (let i = 0; i < 3; i++) { diff += `${String(i).padStart(3)}  MOVE   ${HOME}/Downloads/old-file-${i}.zip\n       →      ${HOME}/FileMind Archive/Downloads/2025/2025-0${i + 3}/old-file-${i}.zip\n`; steps++; }
        diff += "  … 1,201 more\n";
        steps = 1204;
      }
      return { txn_id: `txn_${new Date().toISOString().replace(/[-:]/g, "").slice(0, 15)}_mock`, steps, diff, problems: s.id === 3859 ? [] : [], risk_tier: s.risk_tier, mode, fingerprint: "abcd" };
    }
    case "suggest.apply": {
      const s = suggestions.find((x) => x.id === p.id);
      if (!s) throw new Error(`no proposed suggestion #${p.id}`);
      if (mode === "observe") throw new Error("mode is observe — FileMind only proposes");
      s.state = "accepted";
      const id = String(p.txn_id ?? "txn_mock");
      txns.unshift({ txn_id: id, state: "done", created_ts: now, executed_ts: now + 1, rationale: s.rationale, steps: 2, done: 2 });
      return { txn_id: id, done: 2, failed: 0, state: "done" };
    }
    case "txn.list":
      return txns;
    case "txn.show": {
      const t = txns.find((x) => x.txn_id === p.id);
      if (!t) throw new Error(`unknown transaction ${p.id}`);
      return { manifest: { txn_id: t.txn_id, rationale: t.rationale, steps: [] }, state: t.state, steps: ["done", "done"],
        diff: `     KEEP   ${HOME}/Documents/Offers/OFFER SHEET Calcium.pdf\n  0  TRASH  ${HOME}/Downloads/OFFER SHEET Calcium.pdf\n  1  TRASH  ${HOME}/Downloads/OFFER SHEET Calcium copy.pdf\n` };
    }
    case "txn.undo": {
      const t = txns.find((x) => x.txn_id === p.id);
      if (t) t.state = "undone";
      return { txn_id: p.id, restored: 2, skipped: [] };
    }
    case "mode.get":
      return mode;
    case "mode.set":
      mode = String(p.mode);
      return mode;
    case "search": {
      const q = String(p.query ?? "").toLowerCase();
      const hits = files.filter((f) => !q || f.name.toLowerCase().split(/\W+/).some((w) => q.includes(w) && w.length > 2) || q.includes("offer") && f.name.includes("OFFER"))
        .map((f, i) => ({ ...f, score: 0.03 - i * 0.002, via: i % 2 ? ["semantic#" + (i + 1)] : ["lexical#" + (i + 1), "semantic#" + (i + 2)] }));
      return { query: p.query, parsed: { text: q.replace(/last spring|pdf/g, "").trim(), notes: q.includes("spring") ? ["modified last spring (2026-03-01 – 2026-06-01)", "kind: PDF"] : [] },
        semantic: true, hits: hits.length ? hits : files.slice(0, 3).map((f) => ({ ...f, score: 0.01, via: ["semantic#1"] })), notes: q.includes("accept") ? notes : [], elapsed_ms: 12 };
    }
    case "ask":
      await sleep(900);
      return { answer: "The most recent offer sheet is [1] OFFER SHEET Calcium.pdf in Downloads (modified about four months ago); the supplier named in it is Nordkalk trading. A note on that file says it was the accepted one, signed in May.",
        adapter: ai.adapter === "ollama" ? "ollama-cloud" : ai.adapter, local: ai.adapter === "none", bytes_sent: 1802,
        hits: files.slice(0, 3).map((f, i) => ({ ...f, score: 0.03, via: ["lexical#" + (i + 1)] })) };
    case "archive.list":
      return [{ archive_id: "arc_mock", name: "SaberBattle 5", folder: `${HOME}/Downloads/SaberBattle 5`, pack_path: `${HOME}/FileMind Archive/Projects/SaberBattle 5 (arc_mock).fmpack`, created_ts: now - 3 * day, bytes_raw: 5_700_000_000, bytes_stored: 4_970_000_000, members: 6_362, state: "ready", txn_id: "txn_mock_arc" }];
    case "archive.restore": {
      const f = files.find((x) => x.location === `archive:${p.id}#${p.member}`);
      if (f) { f.status = "present"; delete (f as Record<string, unknown>).location; }
      return { files: 1, bytes: f?.size ?? 0, to: f?.path ?? "" };
    }
    case "notes.add": {
      const n = { note_id: nextNote++, subject_type: p.kind ?? "file", subject_id: p.subject, text: p.text, source: "user", ts: now };
      notes.unshift(n);
      return n;
    }
    case "notes.list":
      return p.subject ? notes.filter((n) => n.subject_id === p.subject) : notes;
    case "notes.remove": {
      const i = notes.findIndex((n) => n.note_id === p.id);
      if (i >= 0) notes.splice(i, 1);
      return { ok: i >= 0 };
    }
    case "ai.get":
      return ai;
    case "ai.set":
      ai = { ...ai, ...(p as typeof ai) };
      return ai;
    case "ai.audit":
      return [
        { id: 4, ts: now - 300, adapter: "ollama-cloud", purpose: "ask", bytes_sent: 1802, file_id: null, local: false, ok: true, latency_ms: 3149 },
        { id: 3, ts: now - 900, adapter: "ollama-cloud", purpose: "ask", bytes_sent: 509, file_id: null, local: false, ok: true, latency_ms: 1126 },
        { id: 2, ts: now - 1500, adapter: "ollama", purpose: "ask", bytes_sent: 0, file_id: null, local: true, ok: true, latency_ms: 1108 },
      ];
    case "info":
      return { path: p.path, category: "contract", sensitive: false };
    case "scan":
      await sleep(1200);
      return [{ path: roots[0], files: 188_177, dirs: 21_004, bytes: 54_700_000_000, new: 3, modified: 1, renamed: 0, moved: 0, missing: 0, unchanged: 188_173, elapsed_ms: 17_000 }];
    case "analyze":
      await sleep(1500);
      return { duplicate_groups: 2630, duplicate_bytes: 37_600_000_000, version_chains: 116, suggestions: suggestions.length, projects: projects.length, health: 72, elapsed_ms: 1450 };
    default:
      throw new Error(`mock: unknown method ${method}`);
  }
}
