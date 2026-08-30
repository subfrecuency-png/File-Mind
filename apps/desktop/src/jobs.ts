// Background jobs (Phase 9): scan, analyze, embed and the model download run
// on the agent's own thread (or the in-process context) and report progress
// through `jobs.status`. This module starts one, shows a progress strip at
// the bottom of the window while it runs, and resolves with the job's result.

import { rpc } from "./api";
import { bytes, errorText, h, num } from "./ui";

export interface Job {
  id: number;
  kind: string;
  state: "running" | "done" | "failed";
  label: string;
  done: number;
  total: number;
  started_ms: number;
  elapsed_ms: number;
  result: unknown;
  error: string | null;
}

let host: HTMLElement | null = null;
const strips = new Map<number, { el: HTMLElement; bar: HTMLElement; text: HTMLElement }>();

function ensureHost() {
  if (!host) {
    host = h("div", { class: "jobs" });
    document.body.appendChild(host);
  }
  return host;
}

function progressText(j: Job): string {
  const unit = j.kind === "model.download" ? bytes : num;
  if (j.total > 0) return `${j.label} · ${unit(j.done)} / ${unit(j.total)}`;
  if (j.done > 0) return `${j.label} · ${unit(j.done)}`;
  return j.label;
}

function render(j: Job) {
  let s = strips.get(j.id);
  if (!s) {
    const bar = h("i");
    const text = h("span", null, j.label);
    const el = h("div", { class: "job" }, h("div", { class: "job-head" }, h("b", null, j.kind), text), h("div", { class: "bar" }, bar));
    ensureHost().appendChild(el);
    s = { el, bar, text };
    strips.set(j.id, s);
  }
  s.text.textContent = progressText(j);
  const pct = j.total > 0 ? Math.min(100, (j.done / j.total) * 100) : j.state === "running" ? 0 : 100;
  s.bar.style.width = `${pct}%`;
  if (j.total === 0 && j.state === "running") s.el.classList.add("indeterminate");
  else s.el.classList.remove("indeterminate");
  if (j.state !== "running") {
    s.el.classList.add(j.state);
    setTimeout(() => {
      s?.el.remove();
      strips.delete(j.id);
    }, j.state === "failed" ? 8000 : 1500);
  }
}

/** Start a job and follow it until it finishes. Rejects with the job's error. */
export async function runJob<T = unknown>(kind: string, params: Record<string, unknown> = {}, onUpdate?: (j: Job) => void): Promise<T> {
  const started = await rpc<Job>("jobs.start", { kind, ...params });
  return followJob<T>(started.id, onUpdate);
}

export async function followJob<T = unknown>(id: number, onUpdate?: (j: Job) => void): Promise<T> {
  for (;;) {
    let j: Job | null;
    try {
      j = await rpc<Job | null>("jobs.status", { id });
    } catch (e) {
      throw new Error(errorText(e));
    }
    if (!j) throw new Error("job vanished (agent restarted?)");
    render(j);
    onUpdate?.(j);
    if (j.state === "done") return j.result as T;
    if (j.state === "failed") throw new Error(j.error ?? "job failed");
    await new Promise((r) => setTimeout(r, 400));
  }
}

/** Re-attach to whatever is running (after navigating or reopening the window). */
export async function resumeRunningJobs(): Promise<void> {
  try {
    const list = await rpc<Job[]>("jobs.list");
    for (const j of list) if (j.state === "running" && !strips.has(j.id)) void followJob(j.id).catch(() => undefined);
  } catch {
    /* no jobs API (older agent) */
  }
}
