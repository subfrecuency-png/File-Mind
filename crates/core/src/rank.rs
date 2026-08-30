//! Reciprocal rank fusion of several ranked lists.
//!
//! score(d) = Σ_lists weight / (k + rank(d)); documents missing from a list
//! contribute nothing from it. k = 60 is the usual constant; it keeps a
//! top-1 in one list from dominating a document that is top-3 in both.

use std::collections::HashMap;

pub const K: f32 = 60.0;

#[derive(Debug, Clone, PartialEq)]
pub struct Fused {
    pub id: String,
    pub score: f32,
    /// (list name, rank) for every list the document appeared in.
    pub sources: Vec<(&'static str, usize)>,
}

/// `lists`: (name, weight, ids in rank order).
pub fn fuse(lists: &[(&'static str, f32, Vec<String>)], limit: usize) -> Vec<Fused> {
    let mut acc: HashMap<&str, Fused> = HashMap::new();
    for (name, weight, ids) in lists {
        for (rank, id) in ids.iter().enumerate() {
            let e = acc.entry(id.as_str()).or_insert_with(|| Fused {
                id: id.clone(),
                score: 0.0,
                sources: Vec::new(),
            });
            e.score += weight / (K + rank as f32 + 1.0);
            e.sources.push((name, rank + 1));
        }
    }
    let mut out: Vec<Fused> = acc.into_values().collect();
    out.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.id.cmp(&b.id)));
    out.truncate(limit);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn agreement_beats_a_single_top_hit() {
        let out = fuse(
            &[
                ("lexical", 1.0, ids(&["a", "b", "c"])),
                ("semantic", 1.0, ids(&["b", "c", "d"])),
            ],
            10,
        );
        assert_eq!(out[0].id, "b");
        assert_eq!(out[0].sources.len(), 2);
        assert_eq!(out.len(), 4);
    }

    #[test]
    fn weights_tilt_the_result() {
        let out = fuse(
            &[
                ("lexical", 2.0, ids(&["a"])),
                ("semantic", 1.0, ids(&["b"])),
            ],
            10,
        );
        assert_eq!(out[0].id, "a");
    }
}
