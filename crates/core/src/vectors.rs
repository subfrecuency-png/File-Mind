//! In-memory vector index: int8-quantised unit vectors, brute-force dot product.
//!
//! For the sizes FileMind sees (10⁵–10⁶ subjects × 384 dims) a linear scan
//! over int8 data is a few tens of milliseconds and needs no extra
//! dependency. If a corpus ever outgrows this, an ANN index slots in behind
//! the same `search` signature.

use std::collections::HashMap;

/// Quantise a unit-length vector to int8 (component × 127).
pub fn quantize(v: &[f32]) -> Vec<i8> {
    v.iter()
        .map(|x| (x * 127.0).round().clamp(-127.0, 127.0) as i8)
        .collect()
}

pub fn dequantize(q: &[i8]) -> Vec<f32> {
    q.iter().map(|&x| x as f32 / 127.0).collect()
}

/// L2-normalise in place; a zero vector is left as is.
pub fn normalize(v: &mut [f32]) {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        for x in v.iter_mut() {
            *x /= n;
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct VecIndex {
    dim: usize,
    ids: Vec<String>,
    data: Vec<i8>,
    pos: HashMap<String, usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub id: String,
    pub score: f32,
}

impl VecIndex {
    pub fn new(dim: usize) -> Self {
        Self {
            dim,
            ..Default::default()
        }
    }

    pub fn dim(&self) -> usize {
        self.dim
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub fn contains(&self, id: &str) -> bool {
        self.pos.contains_key(id)
    }

    /// Insert or replace one vector.
    pub fn upsert(&mut self, id: &str, q: &[i8]) {
        assert_eq!(q.len(), self.dim, "vector dimension");
        if let Some(&i) = self.pos.get(id) {
            self.data[i * self.dim..(i + 1) * self.dim].copy_from_slice(q);
            return;
        }
        self.pos.insert(id.to_string(), self.ids.len());
        self.ids.push(id.to_string());
        self.data.extend_from_slice(q);
    }

    /// Drop a subject. O(1) by swapping the last row in.
    pub fn remove(&mut self, id: &str) {
        let Some(i) = self.pos.remove(id) else {
            return;
        };
        let last = self.ids.len() - 1;
        if i != last {
            let (a, b) = self.data.split_at_mut(last * self.dim);
            a[i * self.dim..(i + 1) * self.dim].copy_from_slice(&b[..self.dim]);
            let moved = self.ids[last].clone();
            self.ids.swap(i, last);
            self.pos.insert(moved, i);
        }
        self.ids.pop();
        self.data.truncate(last * self.dim);
    }

    /// Top-`k` by cosine similarity (inputs are unit vectors, so dot product).
    pub fn search(&self, query: &[f32], k: usize) -> Vec<Hit> {
        if self.is_empty() || k == 0 {
            return Vec::new();
        }
        let q: Vec<i32> = query
            .iter()
            .map(|x| (x * 127.0).round().clamp(-127.0, 127.0) as i32)
            .collect();
        let scale = 1.0 / (127.0 * 127.0);
        let n = self.len();
        let dim = self.dim;
        let score_range = |lo: usize, hi: usize| -> Vec<(f32, usize)> {
            self.data[lo * dim..hi * dim]
                .chunks_exact(dim)
                .enumerate()
                .map(|(i, row)| {
                    let dot: i32 = row.iter().zip(&q).map(|(&a, &b)| a as i32 * b).sum();
                    (dot as f32 * scale, lo + i)
                })
                .collect()
        };
        // Big indexes are scanned on a few threads; the per-thread top-k
        // lists are merged below.
        let threads = if n >= 64 * 1024 {
            std::thread::available_parallelism()
                .map(|p| p.get().min(8))
                .unwrap_or(1)
        } else {
            1
        };
        let mut scored: Vec<(f32, usize)> = if threads <= 1 {
            score_range(0, n)
        } else {
            let per = n.div_ceil(threads);
            std::thread::scope(|sc| {
                let handles: Vec<_> = (0..threads)
                    .map(|t| {
                        let lo = t * per;
                        let hi = ((t + 1) * per).min(n);
                        let f = &score_range;
                        sc.spawn(move || {
                            let mut part = if lo < hi { f(lo, hi) } else { Vec::new() };
                            if part.len() > k {
                                part.select_nth_unstable_by(k - 1, |a, b| b.0.total_cmp(&a.0));
                                part.truncate(k);
                            }
                            part
                        })
                    })
                    .collect();
                handles
                    .into_iter()
                    .flat_map(|h| h.join().unwrap_or_default())
                    .collect()
            })
        };
        let k = k.min(scored.len());
        scored.select_nth_unstable_by(k - 1, |a, b| b.0.total_cmp(&a.0));
        scored.truncate(k);
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        scored
            .into_iter()
            .map(|(score, i)| Hit {
                id: self.ids[i].clone(),
                score,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(seed: u64, dim: usize) -> Vec<f32> {
        let mut x = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let mut v: Vec<f32> = (0..dim)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                ((x % 2000) as f32 / 1000.0) - 1.0
            })
            .collect();
        normalize(&mut v);
        v
    }

    #[test]
    fn finds_nearest_and_survives_removal() {
        let dim = 32;
        let mut ix = VecIndex::new(dim);
        for i in 0..500u64 {
            ix.upsert(&format!("f{i}"), &quantize(&unit(i, dim)));
        }
        let q = unit(123, dim);
        let hits = ix.search(&q, 3);
        assert_eq!(hits[0].id, "f123");
        assert!(hits[0].score > 0.98);
        ix.remove("f123");
        assert!(!ix.contains("f123"));
        assert_eq!(ix.len(), 499);
        let hits = ix.search(&q, 1);
        assert_ne!(hits[0].id, "f123");
        // every remaining id still resolves to its own vector
        for i in [0u64, 1, 250, 499] {
            let id = format!("f{i}");
            if ix.contains(&id) {
                let h = ix.search(&unit(i, dim), 1);
                assert_eq!(h[0].id, id);
            }
        }
    }

    #[test]
    fn quantisation_round_trips_closely() {
        let v = unit(7, 384);
        let back = dequantize(&quantize(&v));
        let dot: f32 = v.iter().zip(&back).map(|(a, b)| a * b).sum();
        assert!(dot > 0.999, "{dot}");
    }

    /// `cargo test -p filemind-core --release vectors::tests::half_a_million -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn half_a_million_vectors_under_150ms() {
        let dim = 384;
        let n = 500_000u64;
        let mut ix = VecIndex::new(dim);
        for i in 0..n {
            ix.upsert(&format!("f{i}"), &quantize(&unit(i, dim)));
        }
        let mut worst = std::time::Duration::ZERO;
        let mut total = std::time::Duration::ZERO;
        for i in 0..20u64 {
            let q = unit(1_000_000 + i, dim);
            let t = std::time::Instant::now();
            let h = ix.search(&q, 300);
            let d = t.elapsed();
            assert_eq!(h.len(), 300);
            worst = worst.max(d);
            total += d;
        }
        eprintln!("500k × {dim}: avg {:?}, worst {:?}", total / 20, worst);
        assert!(worst.as_millis() < 150, "worst {worst:?}");
    }
}
