//! Natural-language query parsing, without a model.
//!
//! "that offer sheet for the calcium supplier from last spring, pdf" becomes
//! a time window (Mar–May of the most recent spring), an extension filter
//! and the residual text "offer sheet calcium supplier" that goes to the
//! lexical and semantic rankers. Everything here is deterministic and runs
//! in microseconds; an LLM adapter can refine the residual later but is
//! never required.

use crate::model::Category;
use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Parsed {
    /// What is left for the rankers after filters are lifted out.
    pub text: String,
    /// Modified on/after (unix seconds).
    pub after: Option<i64>,
    /// Modified before (unix seconds, exclusive).
    pub before: Option<i64>,
    /// Lower-cased extensions without the dot; any of them matches.
    pub exts: Vec<String>,
    pub categories: Vec<Category>,
    /// A folder name the path must contain (lower-cased component).
    pub folder: Option<String>,
    /// A project name hint (matched against project names, case-insensitive).
    pub project: Option<String>,
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
    pub sensitive_only: bool,
    pub duplicates_only: bool,
    /// Human explanation of every filter that was recognised.
    pub notes: Vec<String>,
}

const STOP: &[&str] = &[
    "the",
    "a",
    "an",
    "that",
    "this",
    "those",
    "these",
    "my",
    "our",
    "of",
    "for",
    "to",
    "with",
    "and",
    "or",
    "about",
    "some",
    "any",
    "all",
    "file",
    "files",
    "find",
    "show",
    "me",
    "search",
    "where",
    "is",
    "are",
    "was",
    "were",
    "i",
    "it",
    "which",
    "what",
    "please",
    "one",
    "ones",
    "from",
    "in",
    "on",
    "at",
    "by",
    "into",
    "called",
    "named",
    "regarding",
    "re",
];

fn season_bounds(year: i32, season: &str) -> (NaiveDate, NaiveDate) {
    let d = |y, m, d| NaiveDate::from_ymd_opt(y, m, d).unwrap();
    match season {
        "spring" => (d(year, 3, 1), d(year, 6, 1)),
        "summer" => (d(year, 6, 1), d(year, 9, 1)),
        "fall" | "autumn" => (d(year, 9, 1), d(year, 12, 1)),
        _ => (d(year - 1, 12, 1), d(year, 3, 1)), // winter ending in `year`
    }
}

fn month_num(w: &str) -> Option<u32> {
    let m = [
        ("jan", 1),
        ("feb", 2),
        ("mar", 3),
        ("apr", 4),
        ("may", 5),
        ("jun", 6),
        ("jul", 7),
        ("aug", 8),
        ("sep", 9),
        ("oct", 10),
        ("nov", 11),
        ("dec", 12),
    ];
    let w = w.trim_end_matches('.');
    if w.len() < 3 {
        return None;
    }
    m.iter()
        .find(|(k, _)| w.starts_with(k) && ("january february march april may june july august september october november december".contains(w) || w.len() == 3))
        .map(|(_, n)| *n)
}

fn ts(d: NaiveDate) -> i64 {
    Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0).unwrap())
        .timestamp()
}

fn exts_for(word: &str) -> Option<(&'static [&'static str], Option<Category>, &'static str)> {
    Some(match word {
        "pdf" | "pdfs" => (&["pdf"], None, "PDF"),
        "spreadsheet" | "spreadsheets" | "excel" | "xlsx" | "csv" => (
            &["xlsx", "xls", "csv", "numbers", "ods"],
            Some(Category::Data),
            "spreadsheet",
        ),
        "presentation" | "presentations" | "slides" | "deck" | "decks" | "powerpoint"
        | "keynote" => (&["pptx", "ppt", "key", "odp"], None, "presentation"),
        "word" | "docx" => (
            &["docx", "doc", "pages", "odt", "rtf"],
            None,
            "Word document",
        ),
        "image" | "images" | "photo" | "photos" | "picture" | "pictures" | "pic" | "pics"
        | "jpeg" | "jpg" | "png" => (
            &[
                "jpg", "jpeg", "png", "heic", "gif", "webp", "tiff", "tif", "bmp", "raw", "dng",
            ],
            Some(Category::Photo),
            "image",
        ),
        "screenshot" | "screenshots" => (&["png", "jpg"], Some(Category::Screenshot), "screenshot"),
        "video" | "videos" | "movie" | "movies" | "clip" | "clips" | "mp4" => (
            &["mp4", "mov", "mkv", "avi", "webm", "m4v"],
            Some(Category::Media),
            "video",
        ),
        "audio" | "music" | "song" | "songs" | "mp3" | "recording" | "recordings" => (
            &["mp3", "wav", "m4a", "flac", "aac", "ogg"],
            Some(Category::Media),
            "audio",
        ),
        "zip" | "zips" | "archive" | "archives" | "tarball" => (
            &["zip", "tar", "gz", "tgz", "7z", "rar", "dmg"],
            Some(Category::Archive),
            "archive",
        ),
        "installer" | "installers" => (
            &["dmg", "pkg", "exe", "msi"],
            Some(Category::Installer),
            "installer",
        ),
        "code" | "script" | "scripts" | "source" => (&[], Some(Category::Code), "code"),
        "design" | "designs" | "mockup" | "mockups" | "figma" | "psd" => {
            (&[], Some(Category::Design), "design")
        }
        "invoice" | "invoices" | "receipt" | "receipts" | "bill" | "bills" => {
            (&[], Some(Category::Invoice), "invoice")
        }
        "contract" | "contracts" | "agreement" | "agreements" | "nda" => {
            (&[], Some(Category::Contract), "contract")
        }
        _ => return None,
    })
}

fn size_of(word: &str) -> Option<(Option<u64>, Option<u64>, &'static str)> {
    const MB: u64 = 1 << 20;
    Some(match word {
        "huge" | "enormous" => (Some(500 * MB), None, "over 500 MB"),
        "large" | "big" => (Some(50 * MB), None, "over 50 MB"),
        "small" | "tiny" => (None, Some(100 * 1024), "under 100 KB"),
        _ => return None,
    })
}

fn parse_bytes(num: &str, unit: &str) -> Option<u64> {
    let n: f64 = num.parse().ok()?;
    let mult = match unit.trim_end_matches('s') {
        "kb" | "k" => 1u64 << 10,
        "mb" | "m" | "meg" | "megabyte" => 1 << 20,
        "gb" | "g" | "gig" | "gigabyte" => 1 << 30,
        "b" | "byte" => 1,
        _ => return None,
    };
    Some((n * mult as f64) as u64)
}

/// Parse `q` relative to `now`.
pub fn parse_at(q: &str, now: DateTime<Utc>) -> Parsed {
    let mut p = Parsed::default();
    let today = now.date_naive();
    let words: Vec<String> = q
        .split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric() && c != '.' && c != '-' && c != '_')
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect();
    let mut used = vec![false; words.len()];
    let w = |i: usize| words.get(i).map(String::as_str).unwrap_or("");

    let set_window = |p: &mut Parsed, a: NaiveDate, b: NaiveDate, why: String| {
        p.after = Some(ts(a));
        p.before = Some(ts(b));
        p.notes.push(why);
    };

    let mut i = 0;
    while i < words.len() {
        let cur = w(i);
        // ---- time -------------------------------------------------------
        match cur {
            "today" => {
                set_window(
                    &mut p,
                    today,
                    today + Duration::days(1),
                    "modified today".into(),
                );
                used[i] = true;
            }
            "yesterday" => {
                set_window(
                    &mut p,
                    today - Duration::days(1),
                    today,
                    "modified yesterday".into(),
                );
                used[i] = true;
            }
            "recent" | "recently" | "latest" | "new" | "newest" => {
                p.after = Some(ts(today - Duration::days(30)));
                p.notes.push("modified in the last 30 days".into());
                used[i] = true;
            }
            "old" | "oldest" | "ancient" => {
                p.before = Some(ts(today - Duration::days(365)));
                p.notes.push("not modified for over a year".into());
                used[i] = true;
            }
            "this" | "last" | "past" | "previous" => {
                let nxt = w(i + 1);
                let is_last = cur != "this";
                let mut consumed = 0;
                match nxt {
                    "week" => {
                        let start =
                            today - Duration::days(today.weekday().num_days_from_monday() as i64);
                        let (a, b) = if is_last {
                            (start - Duration::days(7), start)
                        } else {
                            (start, start + Duration::days(7))
                        };
                        set_window(&mut p, a, b, format!("modified {cur} week"));
                        consumed = 2;
                    }
                    "month" => {
                        let first = today.with_day(1).unwrap();
                        let (a, b) = if is_last {
                            let prev = (first - Duration::days(1)).with_day(1).unwrap();
                            (prev, first)
                        } else {
                            let next = if first.month() == 12 {
                                NaiveDate::from_ymd_opt(first.year() + 1, 1, 1).unwrap()
                            } else {
                                first.with_month(first.month() + 1).unwrap()
                            };
                            (first, next)
                        };
                        set_window(&mut p, a, b, format!("modified {cur} month"));
                        consumed = 2;
                    }
                    "year" => {
                        let y = if is_last {
                            today.year() - 1
                        } else {
                            today.year()
                        };
                        set_window(
                            &mut p,
                            NaiveDate::from_ymd_opt(y, 1, 1).unwrap(),
                            NaiveDate::from_ymd_opt(y + 1, 1, 1).unwrap(),
                            format!("modified in {y}"),
                        );
                        consumed = 2;
                    }
                    "spring" | "summer" | "fall" | "autumn" | "winter" => {
                        let (mut a, mut b) = season_bounds(today.year(), nxt);
                        if is_last {
                            // most recent occurrence that has already begun
                            if a > today {
                                let (a2, b2) = season_bounds(today.year() - 1, nxt);
                                a = a2;
                                b = b2;
                            }
                        } else if b < today {
                            // "this winter" said in late year: the coming one
                            let (a2, b2) = season_bounds(today.year() + 1, nxt);
                            a = a2;
                            b = b2;
                        }
                        set_window(&mut p, a, b, format!("modified {cur} {nxt} ({a} – {b})"));
                        consumed = 2;
                    }
                    "few" | "couple" if matches!(w(i + 2), "days" | "weeks" | "months") => {
                        let n = if w(i + 2) == "days" { 3 } else { 2 };
                        let unit = w(i + 2);
                        let days = match unit {
                            "days" => n,
                            "weeks" => n * 7,
                            _ => n * 30,
                        };
                        p.after = Some(ts(today - Duration::days(days)));
                        p.notes.push(format!("modified in the last {n} {unit}"));
                        consumed = 3;
                    }
                    n if n.parse::<i64>().is_ok()
                        && matches!(
                            w(i + 2),
                            "days"
                                | "day"
                                | "weeks"
                                | "week"
                                | "months"
                                | "month"
                                | "years"
                                | "year"
                        ) =>
                    {
                        let n: i64 = n.parse().unwrap();
                        let unit = w(i + 2);
                        let days = match unit.trim_end_matches('s') {
                            "day" => n,
                            "week" => n * 7,
                            "month" => n * 30,
                            _ => n * 365,
                        };
                        p.after = Some(ts(today - Duration::days(days)));
                        p.notes.push(format!("modified in the last {n} {unit}"));
                        consumed = 3;
                    }
                    _ => {}
                }
                if consumed > 0 {
                    for k in 0..consumed {
                        used[i + k] = true;
                    }
                    i += consumed;
                    continue;
                }
            }
            "spring" | "summer" | "fall" | "autumn" | "winter" => {
                // "spring 2025" / "summer of 2024" / bare "spring" (most recent)
                let (year, consumed) = if let Ok(y) = w(i + 1).parse::<i32>() {
                    (y, 2)
                } else if w(i + 1) == "of" && w(i + 2).parse::<i32>().is_ok() {
                    (w(i + 2).parse().unwrap(), 3)
                } else {
                    let (a, _) = season_bounds(today.year(), cur);
                    (
                        if a > today {
                            today.year() - 1
                        } else {
                            today.year()
                        },
                        1,
                    )
                };
                if (1990..=2100).contains(&year) {
                    let (a, b) = season_bounds(year, cur);
                    set_window(&mut p, a, b, format!("modified {cur} {year}"));
                    for k in 0..consumed {
                        used[i + k] = true;
                    }
                    i += consumed;
                    continue;
                }
            }
            _ => {}
        }
        // month names, optionally with a year: "march", "march 2024", "in mar"
        if let Some(m) = month_num(cur) {
            let (year, consumed) = if let Ok(y) = w(i + 1).parse::<i32>() {
                (y, 2)
            } else {
                // most recent occurrence of that month
                let y = if m > today.month() {
                    today.year() - 1
                } else {
                    today.year()
                };
                (y, 1)
            };
            if (1990..=2100).contains(&year) {
                let a = NaiveDate::from_ymd_opt(year, m, 1).unwrap();
                let b = if m == 12 {
                    NaiveDate::from_ymd_opt(year + 1, 1, 1).unwrap()
                } else {
                    NaiveDate::from_ymd_opt(year, m + 1, 1).unwrap()
                };
                set_window(&mut p, a, b, format!("modified in {}", a.format("%B %Y")));
                for k in 0..consumed {
                    used[i + k] = true;
                }
                i += consumed;
                continue;
            }
        }
        // bare year "2024" (not part of a filename-ish token)
        if let Ok(y) = cur.parse::<i32>() {
            if (1990..=2100).contains(&y) && cur.len() == 4 {
                set_window(
                    &mut p,
                    NaiveDate::from_ymd_opt(y, 1, 1).unwrap(),
                    NaiveDate::from_ymd_opt(y + 1, 1, 1).unwrap(),
                    format!("modified in {y}"),
                );
                used[i] = true;
                i += 1;
                continue;
            }
        }
        // "since 2024" / "before 2023" / "after march"
        if matches!(cur, "since" | "after" | "before" | "until") {
            if let Ok(y) = w(i + 1).parse::<i32>() {
                if (1990..=2100).contains(&y) {
                    let t = ts(NaiveDate::from_ymd_opt(y, 1, 1).unwrap());
                    if cur == "before" || cur == "until" {
                        p.before = Some(t);
                    } else {
                        p.after = Some(t);
                    }
                    p.notes.push(format!("modified {cur} {y}"));
                    used[i] = true;
                    used[i + 1] = true;
                    i += 2;
                    continue;
                }
            }
        }
        // ---- kind / category -------------------------------------------
        if let Some((exts, cat, label)) = exts_for(cur) {
            for e in exts {
                if !p.exts.contains(&e.to_string()) {
                    p.exts.push(e.to_string());
                }
            }
            if let Some(c) = cat {
                if !p.categories.contains(&c) {
                    p.categories.push(c);
                }
            }
            p.notes.push(format!("kind: {label}"));
            used[i] = true;
            // keep content words like "invoice" / "contract" in the text too:
            // they help the rankers even when the classifier disagrees.
            if exts.is_empty() {
                used[i] = false;
            }
            i += 1;
            continue;
        }
        // ---- size --------------------------------------------------------
        if let Some((min, max, label)) = size_of(cur) {
            p.min_size = min.or(p.min_size);
            p.max_size = max.or(p.max_size);
            p.notes.push(format!("size {label}"));
            used[i] = true;
            i += 1;
            continue;
        }
        if matches!(
            cur,
            "over" | "above" | "larger" | "bigger" | "under" | "below" | "smaller"
        ) {
            let mut j = i + 1;
            if w(j) == "than" {
                j += 1;
            }
            // "over 100 mb" or "over 100mb"
            let (num, unit, span) = if let Some(k) = w(j).find(|c: char| c.is_alphabetic()) {
                (w(j)[..k].to_string(), w(j)[k..].to_string(), 1)
            } else {
                (w(j).to_string(), w(j + 1).to_string(), 2)
            };
            if let Some(b) = parse_bytes(&num, &unit) {
                if matches!(cur, "over" | "above" | "larger" | "bigger") {
                    p.min_size = Some(b);
                } else {
                    p.max_size = Some(b);
                }
                p.notes.push(format!("size {cur} {num} {unit}"));
                used[i..j + span].iter_mut().for_each(|u| *u = true);
                i = j + span;
                continue;
            }
        }
        // ---- location ----------------------------------------------------
        if matches!(cur, "in" | "inside" | "under" | "from" | "on") {
            let nxt = w(i + 1);
            if matches!(nxt, "project") || (nxt == "the" && w(i + 3) == "project") {
                let (name, span) = if nxt == "project" {
                    (w(i + 2).to_string(), 3)
                } else {
                    (w(i + 2).to_string(), 4)
                };
                if !name.is_empty() {
                    p.project = Some(name.clone());
                    p.notes.push(format!("project: {name}"));
                    for k in 0..span {
                        used[i + k] = true;
                    }
                    i += span;
                    continue;
                }
            }
            let folder = nxt.trim_start_matches("my").to_string();
            let folder = if folder.is_empty() {
                nxt.to_string()
            } else {
                folder
            };
            if matches!(
                folder.as_str(),
                "downloads"
                    | "desktop"
                    | "documents"
                    | "pictures"
                    | "photos"
                    | "movies"
                    | "music"
                    | "trash"
            ) {
                p.folder = Some(folder.clone());
                p.notes.push(format!("folder: {folder}"));
                used[i] = true;
                used[i + 1] = true;
                i += 2;
                continue;
            }
        }
        if cur == "folder" && i > 0 && !used[i - 1] {
            // "<name> folder"
            p.folder = Some(w(i - 1).to_string());
            p.notes.push(format!("folder: {}", w(i - 1)));
            used[i] = true;
            used[i - 1] = true;
        }
        // ---- flags ---------------------------------------------------------
        if matches!(cur, "sensitive" | "secret" | "secrets" | "private") {
            p.sensitive_only = true;
            p.notes.push("sensitive only".into());
            used[i] = true;
        }
        if matches!(cur, "duplicate" | "duplicates" | "dupes" | "copies") {
            p.duplicates_only = true;
            p.notes.push("duplicates only".into());
            used[i] = true;
        }
        i += 1;
    }

    let mut keep: Vec<&str> = Vec::new();
    for (i, wd) in words.iter().enumerate() {
        if used[i] || STOP.contains(&wd.as_str()) {
            continue;
        }
        keep.push(wd);
    }
    p.text = keep.join(" ");
    p
}

pub fn parse(q: &str) -> Parsed {
    parse_at(q, Utc::now())
}

impl Parsed {
    pub fn has_filters(&self) -> bool {
        self.after.is_some()
            || self.before.is_some()
            || !self.exts.is_empty()
            || !self.categories.is_empty()
            || self.folder.is_some()
            || self.project.is_some()
            || self.min_size.is_some()
            || self.max_size.is_some()
            || self.sensitive_only
            || self.duplicates_only
    }

    /// The same query with every filter dropped and all content words kept
    /// as text — what to try when the filtered search finds nothing
    /// ("summer concert" was an event, not a date).
    pub fn relaxed(q: &str) -> Parsed {
        let text = q
            .split_whitespace()
            .map(|w| {
                w.trim_matches(|c: char| !c.is_alphanumeric() && c != '.' && c != '-' && c != '_')
                    .to_lowercase()
            })
            .filter(|w| !w.is_empty() && !STOP.contains(&w.as_str()))
            .collect::<Vec<_>>()
            .join(" ");
        Parsed {
            text,
            notes: vec!["filters relaxed: nothing matched with them".into()],
            ..Default::default()
        }
    }
}

/// Any-word variant of [`fts_expression`] for when the strict form finds nothing.
pub fn fts_expression_any(text: &str) -> String {
    text.split_whitespace()
        .map(|w| format!("\"{}\"", w.replace('"', "")))
        .collect::<Vec<_>>()
        .join(" OR ")
}

/// Escape a residual for FTS5: each word quoted, joined with implicit AND,
/// with prefix matching on the last word so partial names still hit.
pub fn fts_expression(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let n = words.len();
    words
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let w = w.replace('"', "");
            if i + 1 == n && w.len() >= 3 {
                format!("\"{w}\"*")
            } else {
                format!("\"{w}\"")
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 30, 12, 0, 0).unwrap()
    }

    fn day(y: i32, m: u32, d: u32) -> i64 {
        ts(NaiveDate::from_ymd_opt(y, m, d).unwrap())
    }

    #[test]
    fn season_kind_and_residual() {
        let p = parse_at(
            "that offer sheet for the calcium supplier from last spring, pdf",
            now(),
        );
        assert_eq!(p.after, Some(day(2026, 3, 1)));
        assert_eq!(p.before, Some(day(2026, 6, 1)));
        assert_eq!(p.exts, vec!["pdf"]);
        assert_eq!(p.text, "offer sheet calcium supplier");
    }

    #[test]
    fn relative_windows() {
        let p = parse_at("screenshots from last week", now());
        // 2026-08-30 is a Sunday; week starts Mon 08-24; last week = 08-17..08-24
        assert_eq!(p.after, Some(day(2026, 8, 17)));
        assert_eq!(p.before, Some(day(2026, 8, 24)));
        assert!(p.categories.contains(&Category::Screenshot));
        assert_eq!(p.text, "");

        let p = parse_at("invoices last 3 months", now());
        assert_eq!(
            p.after,
            Some(ts(
                NaiveDate::from_ymd_opt(2026, 8, 30).unwrap() - Duration::days(90)
            ))
        );
        assert!(p.categories.contains(&Category::Invoice));
        assert_eq!(p.text, "invoices");

        let p = parse_at("budget spreadsheet march 2024", now());
        assert_eq!(p.after, Some(day(2024, 3, 1)));
        assert_eq!(p.before, Some(day(2024, 4, 1)));
        assert!(p.exts.contains(&"xlsx".to_string()));
        assert_eq!(p.text, "budget");

        let p = parse_at("photos in 2023", now());
        assert_eq!(p.after, Some(day(2023, 1, 1)));
        assert_eq!(p.before, Some(day(2024, 1, 1)));
    }

    #[test]
    fn size_location_project_flags() {
        let p = parse_at("large videos in downloads", now());
        assert_eq!(p.min_size, Some(50 << 20));
        assert_eq!(p.folder.as_deref(), Some("downloads"));
        assert!(p.exts.contains(&"mp4".to_string()));

        let p = parse_at("files over 200 mb", now());
        assert_eq!(p.min_size, Some(200 << 20));
        assert_eq!(p.text, "");

        let p = parse_at("logo in project creditos", now());
        assert_eq!(p.project.as_deref(), Some("creditos"));
        assert_eq!(p.text, "logo");

        let p = parse_at("sensitive duplicates", now());
        assert!(p.sensitive_only && p.duplicates_only);
    }

    #[test]
    fn fts_expression_quotes_and_prefixes() {
        assert_eq!(
            fts_expression("offer sheet calcium"),
            "\"offer\" \"sheet\" \"calcium\"*"
        );
        assert_eq!(fts_expression("q3"), "\"q3\"");
    }
}
