//! Name-based version detection.
//!
//! `report.docx`, `report_v2.docx`, `report (1).docx`, `report copy.docx`,
//! `report FINAL.docx` all normalise to the stem `report`; a set of files in
//! the same folder sharing a normalised stem and extension, where at least
//! one carries a version marker, is a *version chain*. This is deliberately
//! conservative: `invoice-2024.pdf` and `invoice-2025.pdf` are **not** a
//! chain (a 4-digit number is a year, not a version).

/// Marker words that indicate a variant rather than a different document.
const MARKERS: [&str; 12] = [
    "copy", "final", "draft", "new", "old", "latest", "edited", "revised", "backup", "bak", "tmp",
    "temp",
];

/// How confident we are that a stripped suffix meant "a variant of the same document".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Marker {
    /// No suffix stripped.
    None,
    /// A bare trailing number (`report 2`, `photo-03`): could be a version, could be a series.
    Weak,
    /// An explicit marker (`(1)`, `copy`, `_v2`, `final`).
    Strong,
}

/// True if a path component looks like an explicit duplicate marker such as
/// ` (1)`, ` copy`, `_v2`, `-final`.
pub fn has_version_marker(stem: &str) -> bool {
    normalize_stem(stem).1
}

/// Returns `(normalized_stem, had_marker)`.
pub fn normalize_stem(stem: &str) -> (String, bool) {
    let (s, m) = normalize_stem_marker(stem);
    (s, m != Marker::None)
}

/// Full version of [`normalize_stem`] that also reports marker strength.
/// Strong markers strip repeatedly (`report_final_v3` → `report`); a bare
/// number strips only when it is the outermost suffix and nothing else was
/// stripped, so `report-0_v1` stays `report-0` (a series member with a
/// version) instead of collapsing the whole series into one chain.
pub fn normalize_stem_marker(stem: &str) -> (String, Marker) {
    let mut s = stem.trim().to_lowercase();
    let mut marker = Marker::None;
    loop {
        let before = s.clone();
        let mut strong = false;
        let mut weak = false;
        s = strip_one_suffix(&s, &mut strong, &mut weak);
        if weak && !strong {
            if marker != Marker::None {
                // a bare number under a strong marker is part of the name
                s = before;
                break;
            }
            marker = Marker::Weak;
        } else if strong {
            marker = Marker::Strong;
        }
        s = s.trim_end_matches([' ', '-', '_', '.']).to_string();
        if s == before || s.is_empty() {
            if s.is_empty() {
                s = before;
                marker = Marker::None;
            }
            break;
        }
    }
    (s, marker)
}

/// Sort key that orders `v9` before `v10` and `(2)` before `(10)`.
pub fn natural_key(name: &str) -> Vec<(u64, String)> {
    let mut out = Vec::new();
    let mut num = String::new();
    let mut txt = String::new();
    for c in name.to_lowercase().chars() {
        if c.is_ascii_digit() {
            if !txt.is_empty() {
                out.push((u64::MAX, std::mem::take(&mut txt)));
            }
            num.push(c);
        } else {
            if !num.is_empty() {
                out.push((num.parse().unwrap_or(u64::MAX - 1), String::new()));
                num.clear();
            }
            txt.push(c);
        }
    }
    if !num.is_empty() {
        out.push((num.parse().unwrap_or(u64::MAX - 1), String::new()));
    }
    if !txt.is_empty() {
        out.push((u64::MAX, txt));
    }
    out
}

fn strip_one_suffix(s: &str, had: &mut bool, weak: &mut bool) -> String {
    // " (3)" / "(3)"
    if let Some(open) = s.rfind('(') {
        if s.ends_with(')')
            && s[open + 1..s.len() - 1].chars().all(|c| c.is_ascii_digit())
            && s.len() - open <= 6
        {
            *had = true;
            return s[..open].to_string();
        }
    }
    // split off last token on separator
    let Some(idx) = s.rfind([' ', '-', '_']) else {
        return s.to_string();
    };
    let (head, tail) = (&s[..idx], &s[idx + 1..]);
    if head.is_empty() {
        return s.to_string();
    }
    // v2, v12, ver3, version2
    for prefix in ["version", "ver", "v"] {
        if let Some(num) = tail.strip_prefix(prefix) {
            if !num.is_empty() && num.len() <= 3 && num.chars().all(|c| c.is_ascii_digit()) {
                *had = true;
                return head.to_string();
            }
        }
    }
    // copy, copy2, final, draft…
    for m in MARKERS {
        if tail == m
            || (tail.starts_with(m)
                && tail[m.len()..].chars().all(|c| c.is_ascii_digit())
                && tail.len() - m.len() <= 2)
        {
            *had = true;
            return head.to_string();
        }
    }
    // bare small number: "report 2", "report-03" (≤ 3 digits, so years survive)
    if !tail.is_empty() && tail.len() <= 3 && tail.chars().all(|c| c.is_ascii_digit()) {
        *weak = true;
        return head.to_string();
    }
    s.to_string()
}

/// Names that suggest a file was never deliberately named.
pub fn looks_unnamed(name: &str) -> bool {
    let n = name.to_lowercase();
    n.starts_with("untitled")
        || n.starts_with("screenshot")
        || n.starts_with("screen shot")
        || n.starts_with("img_")
        || n.starts_with("dsc_")
        || n.starts_with("image")
        || n.starts_with("document")
        || n.starts_with("new document")
        || n.starts_with("download")
        || n.contains(" (1)")
        || n.contains(" copy")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_common_markers() {
        for (input, want, marker) in [
            ("report", "report", false),
            ("report_v2", "report", true),
            ("report v12", "report", true),
            ("report (1)", "report", true),
            ("report copy", "report", true),
            ("report copy 2", "report", true),
            ("report FINAL", "report", true),
            ("report_final_v3", "report", true),
            ("Report - Draft (2)", "report", true),
            ("invoice-2024", "invoice-2024", false),
            ("invoice 2024", "invoice 2024", false),
            ("photo-03", "photo", true),
            ("report-0_v1", "report-0", true),
            ("v2", "v2", false),
            ("final", "final", false),
        ] {
            let (got, had) = normalize_stem(input);
            assert_eq!(got, want, "stem for {input:?}");
            assert_eq!(had, marker, "marker for {input:?}");
        }
        assert_eq!(normalize_stem_marker("photo-03").1, Marker::Weak);
        assert_eq!(normalize_stem_marker("report (1)").1, Marker::Strong);
        assert_eq!(normalize_stem_marker("report-0_v1").1, Marker::Strong);
        assert_eq!(normalize_stem_marker("report copy 2").1, Marker::Strong);
    }

    #[test]
    fn natural_order() {
        let mut v = vec!["r_v10", "r_v9", "r_v1", "r (2)", "r (10)"];
        v.sort_by_key(|s| natural_key(s));
        assert_eq!(v, ["r (2)", "r (10)", "r_v1", "r_v9", "r_v10"]);
    }

    #[test]
    fn unnamed_detection() {
        assert!(looks_unnamed("Screenshot 2026-08-29 at 01.02.03.png"));
        assert!(looks_unnamed("IMG_4021.HEIC"));
        assert!(looks_unnamed("offer sheet (1).pdf"));
        assert!(!looks_unnamed("OFFER SHEET Calcium.pdf"));
    }
}
