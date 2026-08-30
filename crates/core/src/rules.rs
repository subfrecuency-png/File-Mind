//! Automate-mode rules: the allow-list of what may ever run unattended.
//!
//! A rule kind is tier-0 by construction — reversible (every run is a normal
//! journaled transaction with undo), low blast radius (capped per run, only
//! files nothing has touched for a while, never sensitive files, never
//! inside a project tree or a noise directory), and narrow (each kind does
//! one obvious thing). Everything not listed here stays Assist-only: the
//! agent can propose it, a person has to approve it.
//!
//! Parameters are validated here so a rule can never be stored with a
//! setting that widens its blast radius past the bounds below.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// The allow-list. Adding a kind here is a deliberate, reviewed decision.
pub const KINDS: &[&str] = &[
    ArchiveStaleDownloads::KIND,
    CollapseVersions::KIND,
    TrashExactDuplicates::KIND,
];

/// Hard ceiling on files one run may touch, whatever the rule says.
pub const MAX_ITEMS_CEILING: u64 = 500;

/// Move loose files in Downloads that nothing has touched for a long time
/// into `~/FileMind Archive/Downloads/<year>/<year-month>/`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArchiveStaleDownloads {
    /// Only files older than this (mtime). Minimum 30.
    pub older_than_days: u32,
    /// Oldest first; the rest wait for the next run.
    pub max_items_per_run: u32,
    /// If more than this many files qualify, pause and ask instead of running.
    pub pause_above: u32,
}

impl ArchiveStaleDownloads {
    pub const KIND: &'static str = "archive_stale_downloads";
}

impl Default for ArchiveStaleDownloads {
    fn default() -> Self {
        Self {
            older_than_days: 90,
            max_items_per_run: 50,
            pause_above: 300,
        }
    }
}

/// Tuck older versions of a file (`report_v1..v6`) into a `<name> versions`
/// folder beside the newest one.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CollapseVersions {
    /// Only chains whose members carry explicit markers (`v2`, `copy`,
    /// `final`, `(1)`). Bare-number series such as `photo-03` are left
    /// alone: they are as often a sequence as a version history.
    pub strong_markers_only: bool,
    /// The older versions must be untouched for this long.
    pub older_than_days: u32,
    pub max_items_per_run: u32,
    pub pause_above: u32,
}

impl CollapseVersions {
    pub const KIND: &'static str = "collapse_versions";
}

impl Default for CollapseVersions {
    fn default() -> Self {
        Self {
            strong_markers_only: true,
            older_than_days: 14,
            max_items_per_run: 50,
            pause_above: 300,
        }
    }
}

/// Trash exact (hash-identical) copies of a file that is kept somewhere
/// deliberate. The kept copy is re-hashed at execution; if it differs or is
/// gone, the run stops.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrashExactDuplicates {
    /// Ignore small files: not worth an unattended action.
    pub min_bytes: u64,
    /// The copy that stays must not itself be in Downloads.
    pub keeper_must_be_outside_downloads: bool,
    /// Only trash copies that live in Downloads (the "downloaded it twice" case).
    pub copies_in_downloads_only: bool,
    /// The copy must be untouched for this long.
    pub older_than_days: u32,
    pub max_items_per_run: u32,
    pub pause_above: u32,
}

impl TrashExactDuplicates {
    pub const KIND: &'static str = "trash_exact_duplicates";
}

impl Default for TrashExactDuplicates {
    fn default() -> Self {
        Self {
            min_bytes: 1 << 20,
            keeper_must_be_outside_downloads: true,
            copies_in_downloads_only: true,
            older_than_days: 30,
            max_items_per_run: 50,
            pause_above: 300,
        }
    }
}

/// A validated rule definition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuleKind {
    ArchiveStaleDownloads(ArchiveStaleDownloads),
    CollapseVersions(CollapseVersions),
    TrashExactDuplicates(TrashExactDuplicates),
}

impl RuleKind {
    pub fn kind(&self) -> &'static str {
        match self {
            RuleKind::ArchiveStaleDownloads(_) => ArchiveStaleDownloads::KIND,
            RuleKind::CollapseVersions(_) => CollapseVersions::KIND,
            RuleKind::TrashExactDuplicates(_) => TrashExactDuplicates::KIND,
        }
    }

    pub fn defaults(kind: &str) -> Option<RuleKind> {
        match kind {
            ArchiveStaleDownloads::KIND => Some(RuleKind::ArchiveStaleDownloads(
                ArchiveStaleDownloads::default(),
            )),
            CollapseVersions::KIND => Some(RuleKind::CollapseVersions(CollapseVersions::default())),
            TrashExactDuplicates::KIND => Some(RuleKind::TrashExactDuplicates(
                TrashExactDuplicates::default(),
            )),
            _ => None,
        }
    }

    /// Build from a kind and a (possibly partial) params object: unknown
    /// kinds are refused, missing fields take defaults, and every bound is
    /// checked. The returned params are the complete, normalised object.
    pub fn parse(kind: &str, params: &Value) -> Result<RuleKind, String> {
        let base = Self::defaults(kind).ok_or_else(|| {
            format!(
                "unknown rule kind {kind:?}; tier-0 kinds are: {}",
                KINDS.join(", ")
            )
        })?;
        let mut merged = serde_json::to_value(&base).map_err(|e| e.to_string())?;
        if let (Some(m), Some(p)) = (merged.as_object_mut(), params.as_object()) {
            for (k, v) in p {
                if k == "kind" {
                    continue;
                }
                if !m.contains_key(k) {
                    return Err(format!("unknown parameter {k:?} for {kind}"));
                }
                m.insert(k.clone(), v.clone());
            }
        } else if !params.is_null() && !params.is_object() {
            return Err("params must be an object".into());
        }
        let rule: RuleKind =
            serde_json::from_value(merged).map_err(|e| format!("bad params for {kind}: {e}"))?;
        rule.check()?;
        Ok(rule)
    }

    fn check(&self) -> Result<(), String> {
        let (max_items, pause_above, older) = match self {
            RuleKind::ArchiveStaleDownloads(r) => {
                if r.older_than_days < 30 {
                    return Err(
                        "older_than_days must be at least 30 for unattended archiving".into(),
                    );
                }
                (r.max_items_per_run, r.pause_above, r.older_than_days)
            }
            RuleKind::CollapseVersions(r) => {
                if r.older_than_days < 7 {
                    return Err("older_than_days must be at least 7".into());
                }
                (r.max_items_per_run, r.pause_above, r.older_than_days)
            }
            RuleKind::TrashExactDuplicates(r) => {
                if r.min_bytes < 64 * 1024 {
                    return Err("min_bytes must be at least 65536".into());
                }
                if r.older_than_days < 7 {
                    return Err("older_than_days must be at least 7".into());
                }
                if !r.keeper_must_be_outside_downloads && !r.copies_in_downloads_only {
                    return Err("at least one of keeper_must_be_outside_downloads / copies_in_downloads_only must stay on".into());
                }
                (r.max_items_per_run, r.pause_above, r.older_than_days)
            }
        };
        let _ = older;
        if max_items == 0 || u64::from(max_items) > MAX_ITEMS_CEILING {
            return Err(format!(
                "max_items_per_run must be between 1 and {MAX_ITEMS_CEILING}"
            ));
        }
        if pause_above < max_items {
            return Err("pause_above must be at least max_items_per_run".into());
        }
        Ok(())
    }

    pub fn max_items_per_run(&self) -> u64 {
        u64::from(match self {
            RuleKind::ArchiveStaleDownloads(r) => r.max_items_per_run,
            RuleKind::CollapseVersions(r) => r.max_items_per_run,
            RuleKind::TrashExactDuplicates(r) => r.max_items_per_run,
        })
    }

    pub fn pause_above(&self) -> u64 {
        u64::from(match self {
            RuleKind::ArchiveStaleDownloads(r) => r.pause_above,
            RuleKind::CollapseVersions(r) => r.pause_above,
            RuleKind::TrashExactDuplicates(r) => r.pause_above,
        })
    }

    /// Params without the `kind` tag, for storage and display.
    pub fn params(&self) -> Value {
        let mut v = serde_json::to_value(self).unwrap_or(json!({}));
        if let Some(o) = v.as_object_mut() {
            o.remove("kind");
        }
        v
    }

    /// One-line human description.
    pub fn describe(&self) -> String {
        match self {
            RuleKind::ArchiveStaleDownloads(r) => format!(
                "archive loose Downloads untouched for {} days into ~/FileMind Archive (≤ {} per run)",
                r.older_than_days, r.max_items_per_run
            ),
            RuleKind::CollapseVersions(r) => format!(
                "tuck older versions{} untouched for {} days into a versions folder (≤ {} per run)",
                if r.strong_markers_only { " with explicit markers" } else { "" },
                r.older_than_days,
                r.max_items_per_run
            ),
            RuleKind::TrashExactDuplicates(r) => format!(
                "trash exact copies ≥ {} untouched for {} days{}{} (≤ {} per run)",
                crate::health::human(r.min_bytes),
                r.older_than_days,
                if r.copies_in_downloads_only { " that sit in Downloads" } else { "" },
                if r.keeper_must_be_outside_downloads { ", keeper outside Downloads" } else { "" },
                r.max_items_per_run
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_parse_and_bounds_hold() {
        for k in KINDS {
            let r = RuleKind::parse(k, &json!({})).unwrap();
            assert_eq!(r.kind(), *k);
        }
        assert!(RuleKind::parse("delete_everything", &json!({})).is_err());
        assert!(
            RuleKind::parse("archive_stale_downloads", &json!({"older_than_days": 3})).is_err()
        );
        assert!(RuleKind::parse(
            "archive_stale_downloads",
            &json!({"max_items_per_run": 5000})
        )
        .is_err());
        assert!(RuleKind::parse("archive_stale_downloads", &json!({"bogus": 1})).is_err());
        assert!(RuleKind::parse(
            "trash_exact_duplicates",
            &json!({"keeper_must_be_outside_downloads": false, "copies_in_downloads_only": false})
        )
        .is_err());
        let r = RuleKind::parse("trash_exact_duplicates", &json!({"min_bytes": 2000000})).unwrap();
        assert_eq!(r.params()["min_bytes"], 2000000);
        assert_eq!(r.params()["older_than_days"], 30);
        assert!(r.params().get("kind").is_none());
    }
}
