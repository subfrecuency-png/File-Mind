//! Semantic search and memory: background embedding, hybrid ranking, ask.
//!
//! One `Engine` per process holds the embedder (ONNX when the model is
//! installed, hash fallback otherwise) and the in-memory vector index.
//! Files are embedded from name + path words + category + text head; the
//! text of sensitive files never reaches the embedder.

use anyhow::{Context as _, Result};
use filemind_ai::embed::{self, Embedder};
use filemind_ai::{AiAdapter, Payload, Purpose};
use filemind_core::query::{self, Parsed};
use filemind_core::rank;
use filemind_core::vectors::{quantize, VecIndex};
use filemind_storage::semantic::{FileCard, Note};
use filemind_storage::Db;
use std::collections::HashSet;
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};

pub struct Engine {
    embedder: Mutex<Box<dyn Embedder>>,
    index: RwLock<VecIndex>,
    model_id: String,
    dim: usize,
}

impl Engine {
    /// Open the best available embedder and load its vectors from `db`.
    pub fn open(db: &Db) -> Result<Self> {
        let embedder = embed::open_default()?;
        Self::with_embedder(db, embedder)
    }

    pub fn with_embedder(db: &Db, embedder: Box<dyn Embedder>) -> Result<Self> {
        let model_id = embedder.id().to_string();
        let dim = embedder.dim();
        let t = Instant::now();
        let index = db.load_vec_index(&model_id, dim)?;
        tracing::info!(model = %model_id, vectors = index.len(), ms = t.elapsed().as_millis(), "vector index loaded");
        Ok(Self {
            embedder: Mutex::new(embedder),
            index: RwLock::new(index),
            model_id,
            dim,
        })
    }

    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    pub fn is_semantic(&self) -> bool {
        self.model_id != embed::HashEmbedder::ID
    }

    pub fn vectors(&self) -> usize {
        self.index.read().map(|i| i.len()).unwrap_or(0)
    }

    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        self.embedder
            .lock()
            .map_err(|_| anyhow::anyhow!("embedder poisoned"))?
            .embed(texts)
    }

    fn embed_query(&self, q: &str) -> Result<Vec<f32>> {
        self.embedder
            .lock()
            .map_err(|_| anyhow::anyhow!("embedder poisoned"))?
            .embed_query(q)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct EmbedOpts {
    pub duty_cycle: f32,
    pub max_wall: Option<Duration>,
    pub batch: usize,
}

impl Default for EmbedOpts {
    fn default() -> Self {
        Self {
            duty_cycle: 0.2,
            max_wall: None,
            batch: 32,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, serde::Serialize)]
pub struct EmbedOutcome {
    pub embedded: u64,
    pub unchanged: u64,
    pub notes: u64,
    pub remaining: u64,
    pub elapsed_ms: u128,
}

fn input_hash(s: &str) -> String {
    blake3::hash(s.as_bytes()).to_hex()[..32].to_string()
}

/// Embed files and notes that have no current vector. Duty-cycled like
/// hashing: after each batch the loop sleeps so the embedder keeps to
/// `duty_cycle` of one core.
pub fn embed_pending(db: &Db, engine: &Engine, opts: EmbedOpts) -> Result<EmbedOutcome> {
    let started = Instant::now();
    let mut out = EmbedOutcome::default();
    let duty = opts.duty_cycle.clamp(0.05, 1.0);
    let mut busy = Duration::ZERO;

    // notes first: few, and the user just wrote them
    let notes = db.notes_to_embed(&engine.model_id)?;
    if !notes.is_empty() {
        let texts: Vec<String> = notes.iter().map(note_text).collect();
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        let vecs = engine.embed(&refs)?;
        let rows: Vec<(String, Vec<i8>, String)> = notes
            .iter()
            .zip(vecs.iter())
            .zip(texts.iter())
            .map(|((n, v), t)| (format!("note:{}", n.note_id), quantize(v), input_hash(t)))
            .collect();
        db.put_embeddings(&engine.model_id, &rows)?;
        let mut ix = engine.index.write().unwrap();
        for (s, q, _) in &rows {
            ix.upsert(s, q);
        }
        out.notes += rows.len() as u64;
    }

    loop {
        if opts.max_wall.is_some_and(|w| started.elapsed() >= w) {
            break;
        }
        let cands = db.embed_candidates(&engine.model_id, opts.batch)?;
        if cands.is_empty() {
            break;
        }
        let t = Instant::now();
        let mut texts: Vec<(String, String)> = Vec::with_capacity(cands.len()); // (subject, text)
        for c in &cands {
            let head = match &c.head {
                Some(h) => Some(h.clone()),
                None => {
                    // classified before Phase 7 kept no head: extract once now
                    let h = filemind_core::extract::extract(&c.path, c.size)
                        .ok()
                        .flatten()
                        .map(|t| {
                            filemind_core::classify::head(
                                &t,
                                filemind_storage::semantic::TEXT_HEAD_BYTES,
                            )
                            .to_string()
                        });
                    if let Some(h) = &h {
                        db.put_text_head(&c.subject, c.mtime, h)?;
                    } else {
                        db.put_text_head(&c.subject, c.mtime, "")?;
                    }
                    h.filter(|h| !h.is_empty())
                }
            };
            let words = filemind_storage::inventory::path_tokens(&c.path);
            let text = embed::subject_text(&c.name, &words, c.category.as_deref(), head.as_deref());
            texts.push((c.subject.clone(), text));
        }
        // skip subjects whose input is unchanged (touched files with same text)
        let mut todo: Vec<(String, String, String)> = Vec::new(); // subject, text, hash
        for (subject, text) in texts {
            let h = input_hash(&text);
            if db.embedding_input_hash(&subject)?.as_deref() == Some(h.as_str()) {
                db.touch_embedding(&subject)?;
                out.unchanged += 1;
            } else {
                todo.push((subject, text, h));
            }
        }
        if !todo.is_empty() {
            let refs: Vec<&str> = todo.iter().map(|(_, t, _)| t.as_str()).collect();
            let vecs = engine.embed(&refs)?;
            let rows: Vec<(String, Vec<i8>, String)> = todo
                .iter()
                .zip(vecs.iter())
                .map(|((s, _, h), v)| (s.clone(), quantize(v), h.clone()))
                .collect();
            db.put_embeddings(&engine.model_id, &rows)?;
            let mut ix = engine.index.write().unwrap();
            for (s, q, _) in &rows {
                ix.upsert(s, q);
            }
            out.embedded += rows.len() as u64;
        }
        busy += t.elapsed();
        let target_wall = busy.mul_f32(1.0 / duty);
        let wall = started.elapsed();
        if target_wall > wall {
            std::thread::sleep((target_wall - wall).min(Duration::from_secs(5)));
        }
    }
    out.remaining = db.count_embed_pending(&engine.model_id)?;
    out.elapsed_ms = started.elapsed().as_millis();
    Ok(out)
}

fn note_text(n: &Note) -> String {
    let subject = std::path::Path::new(&n.subject_id)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| n.subject_id.clone());
    format!("note about {subject}: {}", n.text)
}

/// Drop vectors of files that disappeared; called from analysis.
pub fn prune(db: &Db, engine: &Engine) -> Result<usize> {
    let n = db.prune_embeddings()?;
    if n > 0 {
        let fresh = db.load_vec_index(&engine.model_id, engine.dim)?;
        *engine.index.write().unwrap() = fresh;
    }
    Ok(n)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Hybrid,
    Lexical,
    Semantic,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Hit {
    #[serde(flatten)]
    pub card: FileCard,
    pub score: f32,
    /// e.g. ["lexical#2", "semantic#1"]
    pub via: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchResult {
    pub query: String,
    pub parsed: Parsed,
    pub semantic: bool,
    pub hits: Vec<Hit>,
    pub notes: Vec<Note>,
    pub elapsed_ms: u128,
}

const CANDIDATES: usize = 300;

pub fn search(db: &Db, engine: &Engine, q: &str, limit: usize, mode: Mode) -> Result<SearchResult> {
    let started = Instant::now();
    let parsed = query::parse(q);
    let mut r = search_parsed(db, engine, q, parsed, limit, mode)?;
    if r.hits.is_empty() && r.parsed.has_filters() && !r.parsed.text.is_empty() {
        // e.g. "summer concert": the word was an event, not a date
        let relaxed = query::Parsed::relaxed(q);
        let mut r2 = search_parsed(db, engine, q, relaxed, limit, mode)?;
        if !r2.hits.is_empty() {
            r2.elapsed_ms = started.elapsed().as_millis();
            return Ok(r2);
        }
    }
    r.elapsed_ms = started.elapsed().as_millis();
    Ok(r)
}

fn search_parsed(
    db: &Db,
    engine: &Engine,
    q: &str,
    parsed: Parsed,
    limit: usize,
    mode: Mode,
) -> Result<SearchResult> {
    let started = Instant::now();
    let residual = parsed.text.trim();
    let mut lists: Vec<(&'static str, f32, Vec<String>)> = Vec::new();
    let mut note_ids: Vec<i64> = Vec::new();

    if residual.is_empty() {
        lists.push(("filter", 1.0, db.filter_only_ids(&parsed, CANDIDATES)?));
    } else {
        if mode != Mode::Semantic {
            let expr = query::fts_expression(residual);
            let mut ids = db.search_lexical_ids(&expr, CANDIDATES)?;
            let mut notes = db.search_notes_lexical(&expr, 20)?;
            if ids.is_empty() && residual.contains(' ') {
                // a sentence rarely appears word for word: any word, bm25-ranked
                let any = query::fts_expression_any(residual);
                ids = db.search_lexical_ids(&any, CANDIDATES)?;
                if notes.is_empty() {
                    notes = db.search_notes_lexical(&any, 20)?;
                }
            }
            lists.push(("lexical", 1.0, ids));
            note_ids.extend(notes);
        }
        if mode != Mode::Lexical {
            let qv = engine.embed_query(residual)?;
            let ix = engine.index.read().unwrap();
            let hits = ix.search(&qv, CANDIDATES);
            drop(ix);
            // bge scores unrelated text around 0.4–0.6 and a real match 0.65+,
            // so cut relative to the best hit; the hash fallback is flat, keep all
            let top = hits.first().map(|h| h.score).unwrap_or(0.0);
            let floor = if engine.is_semantic() {
                (top - 0.10).max(0.5)
            } else {
                0.0
            };
            let mut ids = Vec::with_capacity(hits.len());
            for h in hits {
                if h.score < floor {
                    break;
                }
                if let Some(n) = h.id.strip_prefix("note:") {
                    if let Ok(n) = n.parse::<i64>() {
                        note_ids.push(n);
                    }
                } else {
                    ids.push(h.id);
                }
            }
            // a weak best match (a question about something not indexed) should
            // not outvote exact words
            let weight = if !engine.is_semantic() {
                0.6
            } else if top < 0.58 {
                0.5
            } else {
                1.0
            };
            lists.push(("semantic", weight, ids));
        }
    }

    // a note that matches vouches for the file it is attached to
    if !note_ids.is_empty() {
        let mut vouched = Vec::new();
        for id in &note_ids {
            if let Some(n) = db.note(*id)? {
                if let Some(fid) = db.file_id_of_path(std::path::Path::new(&n.subject_id))? {
                    if !vouched.contains(&fid) {
                        vouched.push(fid);
                    }
                }
            }
        }
        if !vouched.is_empty() {
            lists.push(("note", 1.5, vouched));
        }
    }
    let fused = rank::fuse(&lists, CANDIDATES);
    let ids: Vec<String> = fused.iter().map(|f| f.id.clone()).collect();
    let cards = db.cards_filtered(&ids, &parsed)?;
    let project_members: Option<HashSet<String>> = match &parsed.project {
        Some(p) => Some(db.project_member_ids(p)?),
        None => None,
    };
    let dup_members: Option<HashSet<String>> = if parsed.duplicates_only {
        Some(db.duplicate_member_ids()?)
    } else {
        None
    };
    let mut hits = Vec::new();
    for card in cards {
        if let Some(m) = &project_members {
            if !m.contains(&card.file_id) {
                continue;
            }
        }
        if let Some(m) = &dup_members {
            if !m.contains(&card.file_id) {
                continue;
            }
        }
        let f = fused.iter().find(|f| f.id == card.file_id).unwrap();
        hits.push(Hit {
            card,
            score: f.score,
            via: f.sources.iter().map(|(n, r)| format!("{n}#{r}")).collect(),
        });
        if hits.len() >= limit {
            break;
        }
    }

    let mut seen = HashSet::new();
    let mut notes = Vec::new();
    for id in note_ids {
        if seen.insert(id) {
            if let Some(n) = db.note(id)? {
                notes.push(n);
            }
        }
        if notes.len() >= 5 {
            break;
        }
    }

    Ok(SearchResult {
        query: q.to_string(),
        parsed,
        semantic: engine.is_semantic(),
        hits,
        notes,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Answer {
    pub answer: String,
    pub adapter: String,
    pub local: bool,
    pub bytes_sent: usize,
    pub hits: Vec<Hit>,
}

/// Answer a question over the top search hits. What goes to the model is
/// name, folder, date and a short excerpt per file — at most 2 KB in total —
/// and every call lands in the audit log.
pub fn ask(db: &Db, engine: &Engine, adapter: &dyn AiAdapter, question: &str) -> Result<Answer> {
    let res = search(db, engine, question, 6, Mode::Hybrid)?;
    let mut ctx = String::new();
    for (i, h) in res.hits.iter().enumerate() {
        let date = chrono::DateTime::from_timestamp(h.card.mtime, 0)
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_default();
        let folder = h
            .card
            .path
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let excerpt = if h.card.sensitive {
            String::new()
        } else {
            db.text_head(&h.card.file_id)?
                .map(|t| filemind_core::classify::head(&t, 220).replace('\n', " "))
                .unwrap_or_default()
        };
        let line = format!(
            "[{}] {} ({folder}, {date}, {}){}\n",
            i + 1,
            h.card.name,
            h.card.category.as_deref().unwrap_or("?"),
            if excerpt.is_empty() {
                String::new()
            } else {
                format!(": {excerpt}")
            }
        );
        if ctx.len() + line.len() > filemind_ai::MAX_SNIPPET_BYTES {
            break;
        }
        ctx.push_str(&line);
    }
    for n in &res.notes {
        let line = format!("[note] {}\n", n.text);
        if ctx.len() + line.len() > filemind_ai::MAX_SNIPPET_BYTES {
            break;
        }
        ctx.push_str(&line);
    }
    let payload = Payload::new("search results", &ctx, Purpose::Ask);
    let instruction = format!(
        "You are FileMind, a local file assistant. Using only the numbered file list below, answer the user's question briefly. \
         Refer to files by their [number] and name. If the list does not contain the answer, say so.\n\nQuestion: {question}"
    );
    if adapter.name() == "none" {
        anyhow::bail!(
            "no AI adapter configured — `filemind ai use ollama` keeps everything on this machine (needs Ollama running); `filemind ai use cloud --key …` is opt-in and audited"
        );
    }
    let t = Instant::now();
    let r = adapter.complete(&payload, &instruction);
    let ok = r.is_ok();
    db.log_ai(
        adapter.name(),
        "ask",
        payload.snippet.len() as u64,
        None,
        &input_hash(&payload.snippet),
        adapter.is_local(),
        ok,
        t.elapsed().as_millis() as i64,
    )?;
    let answer = r.with_context(|| format!("{} adapter", adapter.name()))?;
    Ok(Answer {
        answer,
        adapter: adapter.name().to_string(),
        local: adapter.is_local(),
        bytes_sent: payload.snippet.len(),
        hits: res.hits,
    })
}

/// AI configuration from settings.
pub fn ai_config(db: &Db) -> filemind_ai::llm::Config {
    db.get_setting("ai")
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}

pub fn set_ai_config(db: &Db, c: &filemind_ai::llm::Config) -> Result<()> {
    db.set_setting("ai", &serde_json::to_value(c)?)
}
