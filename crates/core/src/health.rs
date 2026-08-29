//! Health score: a 0–100 number that summarises how much avoidable mess a
//! root (or the whole index) carries. Each component is a ratio in 0..=1
//! multiplied by its weight; the score is 100 minus the penalties. The
//! components are chosen so every one of them maps to a concrete action.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct HealthInputs {
    pub files: u64,
    pub bytes: u64,
    /// Bytes that are exact copies of another file (all but one copy per group).
    pub duplicate_bytes: u64,
    /// Files in folders named like a download landing zone, untouched > 90 days.
    pub stale_downloads: u64,
    pub downloads_files: u64,
    /// Files whose name suggests they were never named (screenshots, `(1)`, `copy`).
    pub unnamed: u64,
    /// Files in version chains that are not the newest member.
    pub orphan_versions: u64,
    /// Files with no or unknown extension.
    pub unclassified: u64,
    /// 0..=1, fraction of the volume that is free. `None` if unknown.
    pub free_fraction: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Component {
    pub name: String,
    pub ratio: f32,
    pub weight: f32,
    pub penalty: f32,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Health {
    pub score: u8,
    pub components: Vec<Component>,
}

fn ratio(n: u64, d: u64) -> f32 {
    if d == 0 {
        0.0
    } else {
        (n as f32 / d as f32).clamp(0.0, 1.0)
    }
}

pub fn score(i: &HealthInputs) -> Health {
    // Ratios are squashed so that a small amount of mess costs little and a
    // lot costs a lot, without ever exceeding the weight.
    let squash = |r: f32| (r * 4.0).min(1.0).sqrt();

    let mut components = vec![
        Component {
            name: "duplicates".into(),
            ratio: ratio(i.duplicate_bytes, i.bytes),
            weight: 25.0,
            penalty: 0.0,
            detail: format!(
                "{} of {} are exact copies",
                human(i.duplicate_bytes),
                human(i.bytes)
            ),
        },
        Component {
            name: "stale_downloads".into(),
            ratio: ratio(i.stale_downloads, i.downloads_files),
            weight: 20.0,
            penalty: 0.0,
            detail: format!(
                "{} of {} files in Downloads untouched for 90+ days",
                i.stale_downloads, i.downloads_files
            ),
        },
        Component {
            name: "naming".into(),
            ratio: ratio(i.unnamed, i.files),
            weight: 15.0,
            penalty: 0.0,
            detail: format!(
                "{} of {} files look unnamed (screenshots, copies, (1))",
                i.unnamed, i.files
            ),
        },
        Component {
            name: "versions".into(),
            ratio: ratio(i.orphan_versions, i.files),
            weight: 15.0,
            penalty: 0.0,
            detail: format!(
                "{} older versions sit next to a newer one",
                i.orphan_versions
            ),
        },
        Component {
            name: "unclassified".into(),
            ratio: ratio(i.unclassified, i.files),
            weight: 10.0,
            penalty: 0.0,
            detail: format!(
                "{} of {} files have no recognisable type",
                i.unclassified, i.files
            ),
        },
    ];
    if let Some(free) = i.free_fraction {
        // pressure starts below 20 % free and is maximal at 5 %
        let pressure = ((0.20 - free) / 0.15).clamp(0.0, 1.0);
        components.push(Component {
            name: "free_space".into(),
            ratio: pressure,
            weight: 15.0,
            penalty: 0.0,
            detail: format!("{:.0}% of the volume is free", free * 100.0),
        });
    }
    let mut total = 0.0;
    for c in &mut components {
        c.penalty = (squash(c.ratio) * c.weight * 10.0).round() / 10.0;
        total += c.penalty;
    }
    Health {
        score: (100.0 - total).round().clamp(0.0, 100.0) as u8,
        components,
    }
}

pub fn human(b: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_tree_scores_100_and_mess_costs_points() {
        let clean = HealthInputs {
            files: 1000,
            bytes: 1 << 30,
            downloads_files: 100,
            ..Default::default()
        };
        assert_eq!(score(&clean).score, 100);

        let messy = HealthInputs {
            duplicate_bytes: 300 << 20,
            stale_downloads: 80,
            unnamed: 200,
            orphan_versions: 50,
            unclassified: 30,
            free_fraction: Some(0.08),
            ..clean
        };
        let h = score(&messy);
        assert!(
            h.score < 40,
            "score {} components {:?}",
            h.score,
            h.components
        );
        assert!(h.components.iter().all(|c| c.penalty <= c.weight));
        assert!(h
            .components
            .iter()
            .any(|c| c.name == "free_space" && c.penalty > 0.0));
    }

    #[test]
    fn score_is_monotone_in_duplicates() {
        let mut prev = 101;
        for dup in [0u64, 10, 50, 100, 300, 600] {
            let h = score(&HealthInputs {
                files: 100,
                bytes: 1000,
                duplicate_bytes: dup,
                ..Default::default()
            });
            assert!(h.score <= prev);
            prev = h.score;
        }
    }
}
