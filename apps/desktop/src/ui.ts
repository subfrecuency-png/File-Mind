// Tiny DOM helpers — enough structure for a handful of screens without a
// framework, and easy to read for whoever picks this up next.

type Child = Node | string | number | null | undefined | false | Child[];

export function h<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  attrs: Record<string, unknown> | null = null,
  ...children: Child[]
): HTMLElementTagNameMap[K] {
  const el = document.createElement(tag);
  if (attrs) {
    for (const [k, v] of Object.entries(attrs)) {
      if (v === null || v === undefined || v === false) continue;
      if (k === "class") el.className = String(v);
      else if (k === "style" && typeof v === "object") Object.assign(el.style, v);
      else if (k.startsWith("on") && typeof v === "function") el.addEventListener(k.slice(2).toLowerCase(), v as EventListener);
      else if (k === "dataset" && typeof v === "object") Object.assign(el.dataset, v);
      else if (v === true) el.setAttribute(k, "");
      else el.setAttribute(k, String(v));
    }
  }
  append(el, children);
  return el;
}

function append(el: Node, children: Child[]) {
  for (const c of children) {
    if (c === null || c === undefined || c === false) continue;
    if (Array.isArray(c)) append(el, c);
    else if (c instanceof Node) el.appendChild(c);
    else el.appendChild(document.createTextNode(String(c)));
  }
}

export function clear(el: Element) {
  while (el.firstChild) el.removeChild(el.firstChild);
}

export function bytes(n: number | undefined | null): string {
  if (!n) return "0 B";
  const u = ["B", "KB", "MB", "GB", "TB"];
  let v = n;
  let i = 0;
  while (v >= 1024 && i < u.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v < 10 && i > 0 ? v.toFixed(1) : Math.round(v)} ${u[i]}`;
}

export function num(n: number | undefined | null): string {
  return (n ?? 0).toLocaleString();
}

export function date(ts: number | undefined | null): string {
  if (!ts) return "—";
  return new Date(ts * 1000).toLocaleDateString(undefined, { year: "numeric", month: "short", day: "numeric" });
}

export function ago(ts: number | undefined | null): string {
  if (!ts) return "never";
  const s = Math.max(0, Math.floor(Date.now() / 1000) - ts);
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  if (s < 86400) return `${Math.floor(s / 3600)} h ago`;
  const d = Math.floor(s / 86400);
  if (d < 30) return `${d} day${d === 1 ? "" : "s"} ago`;
  if (d < 365) {
    const m = Math.floor(d / 30);
    return `${m} month${m === 1 ? "" : "s"} ago`;
  }
  return `${(d / 365).toFixed(1)} years ago`;
}

/** "~/Downloads/x.pdf" for display; full path stays in the title attribute. */
export function shortPath(p: string): string {
  return p.replace(/^\/Users\/[^/]+/, "~").replace(/^C:\\Users\\[^\\]+/, "~");
}

export function parentDir(p: string): string {
  const i = Math.max(p.lastIndexOf("/"), p.lastIndexOf("\\"));
  return i > 0 ? p.slice(0, i) : p;
}

export function baseName(p: string): string {
  const i = Math.max(p.lastIndexOf("/"), p.lastIndexOf("\\"));
  return i >= 0 ? p.slice(i + 1) : p;
}

export function button(label: string, onClick: () => void, opts: { kind?: "primary" | "danger" | "ghost"; small?: boolean; title?: string } = {}) {
  return h("button", { class: `btn ${opts.kind ?? ""} ${opts.small ? "small" : ""}`, onClick, title: opts.title }, label);
}

let toastHost: HTMLElement | null = null;
export function toast(msg: string, kind: "info" | "error" | "ok" = "info") {
  if (!toastHost) {
    toastHost = h("div", { class: "toasts" });
    document.body.appendChild(toastHost);
  }
  const t = h("div", { class: `toast ${kind}` }, msg);
  toastHost.appendChild(t);
  setTimeout(() => t.classList.add("show"), 10);
  setTimeout(() => {
    t.classList.remove("show");
    setTimeout(() => t.remove(), 300);
  }, kind === "error" ? 7000 : 3500);
}

export function errorText(e: unknown): string {
  if (typeof e === "string") return e;
  if (e && typeof e === "object" && "message" in e) return String((e as { message: unknown }).message);
  return String(e);
}

/** A modal that resolves when closed. `body` may hold its own buttons. */
export function modal(title: string, body: HTMLElement, actions: HTMLElement[] = []): { close: () => void } {
  const closeAll = () => overlay.remove();
  const overlay = h(
    "div",
    { class: "overlay", onClick: (e: Event) => e.target === overlay && closeAll() },
    h(
      "div",
      { class: "modal" },
      h("div", { class: "modal-head" }, h("h2", null, title), button("✕", closeAll, { kind: "ghost", small: true })),
      h("div", { class: "modal-body" }, body),
      actions.length ? h("div", { class: "modal-actions" }, actions) : null,
    ),
  );
  document.body.appendChild(overlay);
  return { close: closeAll };
}

export function spinner(label = "Loading…") {
  return h("div", { class: "spinner" }, h("span", { class: "dot" }), label);
}

export function empty(text: string) {
  return h("div", { class: "empty" }, text);
}

export function pill(text: string, cls = "") {
  return h("span", { class: `pill ${cls}` }, text);
}
