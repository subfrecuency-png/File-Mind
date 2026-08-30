# FileMind desktop

A Tauri 2 window over the same JSON-RPC the CLI uses. Screens: Overview
(health, index, what is in here), Search (natural language, Ask), Projects,
Approvals (preview → apply, every plan shows KEEP / TRASH / MOVE lines),
History (undo), Settings (mode, folders, AI adapter, agent, audit log), and
a first-run onboarding.

## Run in development

Requires Node 22+ and the Rust toolchain (already set up for the CLI).

```sh
cd apps/desktop
npm install
npm run tauri dev
```

The app talks to a running `filemind-agent` of the same build when there is
one; otherwise it handles every request in-process against the database
(search, projects, approvals, undo all work — only live watching needs the
agent). Settings → Background agent can start it; it looks for the binary
next to the app, in `FILEMIND_AGENT_BIN`, or in the repo's `target/debug`.

`npm run dev` alone serves the UI in a normal browser with mock data — handy
for working on screens without the daemon.

## Build a bundle

```sh
npm run tauri build
```

Signing, notarization, sidecar bundling of the agent and auto-update are
Phase 9.
