# Telemetry — what is sent, when, and by whom

Telemetry is **off by default**. Settings → Feedback & diagnostics → "Share aggregate usage" turns it on; the same screen shows the exact document that will go out. When it is on, the agent sends **one JSON document per day**, for the previous complete UTC day, to the endpoint in the `telemetry.endpoint` setting (empty until the beta backend exists — with an empty endpoint nothing is sent even when the toggle is on).

The document is built only from two tables of counters (`metrics`, `sessions`) and a handful of totals. It never contains a path, a file name, a hash, extracted text, a query, or anything an adapter was sent. The field list below is pinned by a test (`crates/agent/src/telemetry.rs::tests::fields_match_the_docs`): adding a field means editing this table.

| Field | Example | Why it is collected |
|---|---|---|
| `schema` | `1` | versioning the document |
| `install_id` | `"9f1c…"` (16 random bytes, hex) | count installs, not people; a **new id is minted every time telemetry is switched on**, so opting out and back in never links the two periods |
| `day` | `"2026-08-29"` | which UTC day the counts cover |
| `version` | `"0.2.0"` | which build the numbers belong to |
| `os` | `"macos"` | platform mix |
| `arch` | `"aarch64"` | Apple silicon vs Intel |
| `health_bucket` | `"70-79"` | is the score in a useful range (`<50`, `50-59`, … `90+`) |
| `files_bucket` | `"100k-250k"` | index sizes we must keep fast (`<10k` … `1M+`) |
| `suggestions_applied` | `3` | user-approved transactions that day |
| `suggestions_undone` | `0` | how often people reverse them |
| `rules_armed` | `1` | Automate rules armed that day |
| `rule_runs` | `2` | transactions executed by rules |
| `sessions` | `4` | agent/app process runs started that day |
| `crash_free_sessions` | `4` | of those, how many ended cleanly (or are still running); the beta target is ≥ 99 % |
| `undo_failures` | `0` | undos that could not put at least one file back — the "data loss" definition for the beta; the target is 0 |
| `adapter` | `"none"` / `"ollama"` / `"cloud"` | which Ask adapters are actually used |

Requests carry no cookies or other identifiers. The receiving side keeps request logs (IP addresses) for 24 hours for abuse protection and nothing longer.

## Crash reports

Separate from telemetry, and never automatic. If the agent, the app or the CLI panics, a plain-text report is written to `<data dir>/crashes/` (macOS: `~/Library/Application Support/FileMind/crashes/`) with the version, OS, component, the panic message and a backtrace in which every path under the home folder is replaced by `~`. Settings → Feedback & diagnostics lists pending reports; you can read one, attach it to a feedback message (a pre-filled GitHub issue or email), or dismiss it. Dismissed reports are renamed, not deleted.
