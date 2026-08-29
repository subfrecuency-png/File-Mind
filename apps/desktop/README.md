# FileMind desktop (Tauri)

Phase 8. Scaffold with:

```sh
npm create tauri-app@latest . -- --template svelte-ts
```

The GUI talks only to `filemind-agent` over the local JSON-RPC socket; it never touches the file system directly.
