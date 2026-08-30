//! Text embeddings. The real thing is `bge-small-en-v1.5` through ONNX
//! Runtime; `HashEmbedder` is a dependency-free stand-in for tests and for
//! machines where the model has not been downloaded (search then degrades
//! to lexical + a crude bag-of-ngrams similarity rather than failing).

use anyhow::{Context, Result};
use filemind_core::vectors::normalize;
use std::path::{Path, PathBuf};

pub const BGE_SMALL: ModelSpec = ModelSpec {
    id: "bge-small-en-v1.5",
    dim: 384,
    query_prefix: "Represent this sentence for searching relevant passages: ",
    max_tokens: 512,
    files: &[
        (
            "model.onnx",
            "https://huggingface.co/BAAI/bge-small-en-v1.5/resolve/main/onnx/model.onnx",
            "a85e4bee6a35baf214890310353795aa2bd220484d6237db953e841d7b10cb20",
        ),
        (
            "tokenizer.json",
            "https://huggingface.co/BAAI/bge-small-en-v1.5/resolve/main/tokenizer.json",
            "6e933bf59db40b8b2a0de480fe5006662770757e1e1671eb7e48ff6a5f00b0b4",
        ),
    ],
};

#[derive(Debug, Clone, Copy)]
pub struct ModelSpec {
    pub id: &'static str,
    pub dim: usize,
    /// bge models want this in front of *queries* (not documents).
    pub query_prefix: &'static str,
    pub max_tokens: usize,
    /// (file name, download URL, blake3 hex)
    pub files: &'static [(&'static str, &'static str, &'static str)],
}

impl ModelSpec {
    /// `<data dir>/models/<id>/`
    pub fn dir(&self) -> Result<PathBuf> {
        let dirs = directories::ProjectDirs::from("", "", "FileMind")
            .context("no per-user data directory")?;
        Ok(dirs.data_local_dir().join("models").join(self.id))
    }

    pub fn is_installed(&self) -> bool {
        self.dir()
            .map(|d| self.files.iter().all(|(f, _, _)| d.join(f).is_file()))
            .unwrap_or(false)
    }

    /// Download and verify every file. Existing verified files are kept.
    pub fn download(&self, progress: &mut dyn FnMut(&str, u64, u64)) -> Result<PathBuf> {
        let dir = self.dir()?;
        std::fs::create_dir_all(&dir)?;
        for (name, url, hash) in self.files {
            let dest = dir.join(name);
            if dest.is_file() && file_hash(&dest)? == *hash {
                continue;
            }
            let tmp = dir.join(format!("{name}.part"));
            let resp = ureq::get(*url)
                .call()
                .with_context(|| format!("downloading {url}"))?;
            let total = resp
                .headers()
                .get("content-length")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0);
            let mut reader = resp.into_body().into_reader();
            let mut out = std::fs::File::create(&tmp)?;
            let mut got = 0u64;
            let mut buf = vec![0u8; 1 << 20];
            loop {
                let n = std::io::Read::read(&mut reader, &mut buf)?;
                if n == 0 {
                    break;
                }
                std::io::Write::write_all(&mut out, &buf[..n])?;
                got += n as u64;
                progress(name, got, total);
            }
            drop(out);
            let h = file_hash(&tmp)?;
            if h != *hash {
                anyhow::bail!(
                    "{name}: checksum mismatch (got {h}, expected {hash}); download left at {}",
                    tmp.display()
                );
            }
            std::fs::rename(&tmp, &dest)?;
        }
        Ok(dir)
    }
}

fn file_hash(p: &Path) -> Result<String> {
    let mut h = blake3::Hasher::new();
    h.update_reader(std::fs::File::open(p)?)?;
    Ok(h.finalize().to_hex().to_string())
}

pub trait Embedder: Send {
    fn id(&self) -> &str;
    fn dim(&self) -> usize;
    /// Unit-length vectors, one per input, in order.
    fn embed(&mut self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;
    /// Embed a *query* (models may want a prefix here).
    fn embed_query(&mut self, q: &str) -> Result<Vec<f32>> {
        Ok(self.embed(&[q])?.remove(0))
    }
}

/// Feature-hashed word + character-trigram bag, L2-normalised. No model
/// needed; catches shared words and morphology, not meaning.
pub struct HashEmbedder {
    dim: usize,
}

impl HashEmbedder {
    pub const ID: &'static str = "hash-v1";

    pub fn new(dim: usize) -> Self {
        Self { dim }
    }

    fn slot(&self, s: &str, seed: u64) -> (usize, f32) {
        let mut h = blake3::Hasher::new();
        h.update(&seed.to_le_bytes());
        h.update(s.as_bytes());
        let b = h.finalize();
        let x = u64::from_le_bytes(b.as_bytes()[..8].try_into().unwrap());
        let sign = if b.as_bytes()[8] & 1 == 0 { 1.0 } else { -1.0 };
        ((x % self.dim as u64) as usize, sign)
    }
}

impl Embedder for HashEmbedder {
    fn id(&self) -> &str {
        Self::ID
    }
    fn dim(&self) -> usize {
        self.dim
    }
    fn embed(&mut self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|t| {
                let mut v = vec![0f32; self.dim];
                let lower = t.to_lowercase();
                for w in lower
                    .split(|c: char| !c.is_alphanumeric())
                    .filter(|w| w.len() > 1)
                {
                    let (i, s) = self.slot(w, 1);
                    v[i] += 2.0 * s;
                    let chars: Vec<char> = w.chars().collect();
                    if chars.len() >= 4 {
                        for g in chars.windows(3) {
                            let g: String = g.iter().collect();
                            let (i, s) = self.slot(&g, 2);
                            v[i] += 0.5 * s;
                        }
                    }
                }
                normalize(&mut v);
                v
            })
            .collect())
    }
}

#[cfg(feature = "onnx")]
pub use onnx::OnnxEmbedder;

#[cfg(feature = "onnx")]
mod onnx {
    use super::*;
    use ort::session::{builder::GraphOptimizationLevel, Session};
    use ort::value::Tensor;
    use tokenizers::Tokenizer;

    pub struct OnnxEmbedder {
        spec: ModelSpec,
        session: Session,
        tokenizer: Tokenizer,
    }

    impl OnnxEmbedder {
        pub fn load(spec: ModelSpec) -> Result<Self> {
            let dir = spec.dir()?;
            Self::load_from(spec, &dir)
        }

        pub fn load_from(spec: ModelSpec, dir: &Path) -> Result<Self> {
            let model = dir.join("model.onnx");
            let tok = dir.join("tokenizer.json");
            if !model.is_file() || !tok.is_file() {
                anyhow::bail!(
                    "embedding model {} not installed in {} — run `filemind model download`",
                    spec.id,
                    dir.display()
                );
            }
            // ort's builder errors carry the builder and are not Send; stringify.
            let session = Session::builder()
                .map_err(|e| anyhow::anyhow!("onnx runtime: {e}"))?
                .with_optimization_level(GraphOptimizationLevel::Level3)
                .map_err(|e| anyhow::anyhow!("onnx runtime: {e}"))?
                .with_intra_threads(2)
                .map_err(|e| anyhow::anyhow!("onnx runtime: {e}"))?
                .commit_from_file(&model)
                .map_err(|e| anyhow::anyhow!("loading {}: {e}", model.display()))?;
            let mut tokenizer =
                Tokenizer::from_file(&tok).map_err(|e| anyhow::anyhow!("tokenizer: {e}"))?;
            tokenizer
                .with_truncation(Some(tokenizers::TruncationParams {
                    max_length: spec.max_tokens,
                    ..Default::default()
                }))
                .map_err(|e| anyhow::anyhow!("tokenizer: {e}"))?;
            Ok(Self {
                spec,
                session,
                tokenizer,
            })
        }
    }

    impl Embedder for OnnxEmbedder {
        fn id(&self) -> &str {
            self.spec.id
        }
        fn dim(&self) -> usize {
            self.spec.dim
        }
        fn embed(&mut self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
            if texts.is_empty() {
                return Ok(Vec::new());
            }
            let enc = self
                .tokenizer
                .encode_batch(texts.to_vec(), true)
                .map_err(|e| anyhow::anyhow!("tokenize: {e}"))?;
            let n = enc.len();
            let len = enc
                .iter()
                .map(|e| e.get_ids().len())
                .max()
                .unwrap_or(1)
                .max(1);
            let mut ids = vec![0i64; n * len];
            let mut mask = vec![0i64; n * len];
            let tt = vec![0i64; n * len];
            for (i, e) in enc.iter().enumerate() {
                for (j, (&id, &m)) in e.get_ids().iter().zip(e.get_attention_mask()).enumerate() {
                    ids[i * len + j] = id as i64;
                    mask[i * len + j] = m as i64;
                }
            }
            let e = |e: ort::Error| anyhow::anyhow!("onnx: {e}");
            let out = self
                .session
                .run(ort::inputs![
                    "input_ids" => Tensor::from_array(([n, len], ids)).map_err(e)?,
                    "attention_mask" => Tensor::from_array(([n, len], mask)).map_err(e)?,
                    "token_type_ids" => Tensor::from_array(([n, len], tt)).map_err(e)?,
                ])
                .map_err(e)?;
            let (shape, data) = out[0].try_extract_tensor::<f32>().map_err(e)?;
            let dim = shape[2] as usize;
            anyhow::ensure!(dim == self.spec.dim, "model dim {dim} != {}", self.spec.dim);
            Ok((0..n)
                .map(|i| {
                    // CLS pooling, as bge is trained for
                    let mut v = data[i * len * dim..i * len * dim + dim].to_vec();
                    normalize(&mut v);
                    v
                })
                .collect())
        }
        fn embed_query(&mut self, q: &str) -> Result<Vec<f32>> {
            let s = format!("{}{q}", self.spec.query_prefix);
            Ok(self.embed(&[&s])?.remove(0))
        }
    }
}

/// Pick the best available embedder: ONNX if the model is installed, else the hash fallback.
pub fn open_default() -> Result<Box<dyn Embedder>> {
    #[cfg(feature = "onnx")]
    {
        if BGE_SMALL.is_installed() {
            return Ok(Box::new(OnnxEmbedder::load(BGE_SMALL)?));
        }
    }
    Ok(Box::new(HashEmbedder::new(BGE_SMALL.dim)))
}

/// What the subject text of a file looks like: name, path words, category,
/// then the first part of its content. Same function for indexing and for
/// tests so the two never drift.
pub fn subject_text(
    name: &str,
    path_words: &str,
    category: Option<&str>,
    head: Option<&str>,
) -> String {
    let mut s = String::with_capacity(256 + head.map(str::len).unwrap_or(0));
    s.push_str(name);
    s.push_str(" — ");
    s.push_str(path_words);
    if let Some(c) = category {
        s.push_str(" — ");
        s.push_str(c);
    }
    if let Some(h) = head {
        s.push('\n');
        s.push_str(h);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_embedder_is_unit_and_sees_shared_words() {
        let mut e = HashEmbedder::new(128);
        let v = e
            .embed(&["offer sheet calcium", "calcium offer", "cat video"])
            .unwrap();
        for x in &v {
            let n: f32 = x.iter().map(|a| a * a).sum::<f32>().sqrt();
            assert!((n - 1.0).abs() < 1e-4);
        }
        let dot = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
        assert!(dot(&v[0], &v[1]) > dot(&v[0], &v[2]));
    }

    /// Needs the real model: `FILEMIND_MODEL_DIR=/path/with/model.onnx cargo test -p filemind-ai -- --ignored`
    #[test]
    #[ignore]
    #[cfg(feature = "onnx")]
    fn onnx_embedder_ranks_paraphrases() {
        let dir = std::env::var("FILEMIND_MODEL_DIR").expect("FILEMIND_MODEL_DIR");
        let mut e = OnnxEmbedder::load_from(BGE_SMALL, Path::new(&dir)).unwrap();
        let docs = e
            .embed(&[
                "offer sheet calcium supplier.pdf — downloads — document\nCalcium carbonate offer, pricing per ton, delivery terms",
                "vacation photo.jpg — pictures — photo",
                "q3 budget.xlsx — documents finance — data\nrevenue forecast spreadsheet",
            ])
            .unwrap();
        let q = e
            .embed_query("pricing quote from the calcium vendor")
            .unwrap();
        let dot = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
        let s: Vec<f32> = docs.iter().map(|d| dot(&q, d)).collect();
        assert!(s[0] > s[1] && s[0] > s[2], "{s:?}");
        assert_eq!(e.dim(), 384);
    }
}
