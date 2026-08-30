// One door to the backend. Inside Tauri every call goes through the `rpc`
// command (agent socket, or in-process fallback). In a plain browser —
// `npm run dev` opened directly, or the screenshot harness — a mock answers,
// so the UI can be developed and reviewed without the daemon.

import { mockRpc } from "./mock";

type Invoke = (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;

const tauriInternals = (window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
export const isTauri = !!tauriInternals;

let invokeFn: Invoke | null = null;
async function invoke(cmd: string, args?: Record<string, unknown>): Promise<unknown> {
  if (!invokeFn) {
    const m = await import("@tauri-apps/api/core");
    invokeFn = m.invoke as Invoke;
  }
  return invokeFn(cmd, args);
}

export async function rpc<T = unknown>(method: string, params: Record<string, unknown> = {}): Promise<T> {
  if (!isTauri) return mockRpc(method, params) as Promise<T>;
  return (await invoke("rpc", { method, params })) as T;
}

export interface AgentInfo {
  running: boolean;
  stale: boolean;
  binary: string | null;
  build: string;
}

export async function agentInfo(): Promise<AgentInfo> {
  if (!isTauri) return { running: true, stale: false, binary: "/mock/filemind-agent", build: "mock" };
  return (await invoke("agent_info")) as AgentInfo;
}

export async function agentStart(): Promise<unknown> {
  if (!isTauri) return { started: true };
  return invoke("agent_start");
}

export async function agentStop(): Promise<boolean> {
  if (!isTauri) return true;
  return (await invoke("agent_stop")) as boolean;
}

/** Forget the in-process context so the next call re-opens the database and engine. */
export async function localReset(): Promise<void> {
  if (!isTauri) return;
  await invoke("local_reset");
}

export interface Autostart { supported: boolean; enabled: boolean; program: string | null; plist: string | null }

export async function autostartGet(): Promise<Autostart> {
  if (!isTauri) return { supported: true, enabled: true, program: "/Applications/FileMind.app/Contents/MacOS/filemind-agent", plist: "~/Library/LaunchAgents/ai.filemind.agent.plist" };
  return (await invoke("autostart_get")) as Autostart;
}

export async function autostartSet(enabled: boolean): Promise<Autostart> {
  if (!isTauri) return { supported: true, enabled, program: "/Applications/FileMind.app/Contents/MacOS/filemind-agent", plist: null };
  return (await invoke("autostart_set", { enabled })) as Autostart;
}

export interface UpdateInfo { available: boolean; current: string; version?: string; date?: string | null; notes?: string | null }

export async function updateCheck(): Promise<UpdateInfo> {
  if (!isTauri) return { available: false, current: "0.1.0 (mock)" };
  return (await invoke("update_check")) as UpdateInfo;
}

export async function updateInstall(): Promise<unknown> {
  if (!isTauri) return { installed: false };
  return invoke("update_install");
}

export async function feedbackUrl(kind: "feedback" | "bug" | "crash", report?: string): Promise<string> {
  if (!isTauri) return `https://github.com/subfrecuency/filemind/issues/new?title=${encodeURIComponent("Feedback: ")}`;
  return (await invoke("feedback_url", { kind, report: report ?? null })) as string;
}

export async function openUrl(url: string): Promise<void> {
  if (!isTauri) {
    window.open(url, "_blank");
    return;
  }
  const { openUrl: ou } = await import("@tauri-apps/plugin-opener");
  await ou(url);
}

/** Native folder picker; null when cancelled. In the browser, a prompt. */
export async function pickFolder(): Promise<string | null> {
  if (!isTauri) return window.prompt("Folder path to add", "/Users/you/Documents");
  const { open } = await import("@tauri-apps/plugin-dialog");
  const r = await open({ directory: true, multiple: false, title: "Choose a folder for FileMind to index" });
  return typeof r === "string" ? r : null;
}

export async function reveal(path: string): Promise<void> {
  if (!isTauri) {
    console.log("reveal", path);
    return;
  }
  const { revealItemInDir } = await import("@tauri-apps/plugin-opener");
  await revealItemInDir(path);
}

export async function openPath(path: string): Promise<void> {
  if (!isTauri) {
    console.log("open", path);
    return;
  }
  const { openPath: op } = await import("@tauri-apps/plugin-opener");
  await op(path);
}
