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
