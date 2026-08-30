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
npm run tauri build          # → src-tauri/target/release/bundle/dmg/FileMind_*.dmg
```

`tauri build` first runs `scripts/build-sidecar.sh --release`, which compiles
`filemind-agent` and the `filemind` CLI and drops them in
`src-tauri/binaries/<name>-<triple>`; Tauri copies them into
`FileMind.app/Contents/MacOS/` next to the app binary (`tauri dev` does the
same into `target/debug/`, so the dev app also finds its sidecar). ORT is
linked statically, so no dylib rides along; the script checks with `otool`.

### Start at login (launchd)

Settings → Background agent → "Start at login" writes
`~/Library/LaunchAgents/ai.filemind.agent.plist` pointing at the bundled
sidecar, with `KeepAlive` (relaunch on crash, not on a clean stop) and
`ThrottleInterval` 30 s; logs go to `~/Library/Logs/FileMind/agent.log`.
Every app launch re-checks the plist and rewrites it if the sidecar path
changed (the app was moved or updated). While the plist exists "Start agent"
asks launchd (`launchctl kickstart`) instead of spawning a child, so the agent
survives quitting the app. "Stop" writes the stop file; the agent also exits
cleanly on SIGTERM (`launchctl bootout`).

### Signing, notarization, updates

`.github/workflows/release.yml` builds both Apple targets on a `v*` tag,
signs + notarizes with the Developer ID secrets listed at the top of the
file, signs the updater artifact and publishes a draft GitHub Release with
`latest.json`. One-time setup:

```sh
cargo install tauri-cli --version "^2"   # or npx tauri
npx tauri signer generate -w ~/.tauri/filemind.key
# paste the printed public key into src-tauri/tauri.conf.json → plugins.updater.pubkey
# add the private key + password as TAURI_SIGNING_PRIVATE_KEY(_PASSWORD) secrets
```

Settings → Updates → "Check for updates" reads
`https://github.com/subfrecuency/filemind/releases/latest/download/latest.json`;
"Install and relaunch" downloads, verifies the minisign signature, swaps the
bundle and restarts. The running agent is stopped first; the relaunched app
starts the new sidecar (a stale-build agent is refused anyway).

A local `npm run tauri build` needs `TAURI_SIGNING_PRIVATE_KEY` in the
environment while `createUpdaterArtifacts` is on, or set it to `false`
temporarily for an unsigned local bundle.

### Windows

The Windows adapter (Recycle Bin, USN journal, Task Scheduler) is still a
stub set. FileMind is macOS-first for the beta; the Windows CI job only
keeps the core crates compiling there.
