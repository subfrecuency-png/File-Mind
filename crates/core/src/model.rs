use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Stable identity of a file across renames and moves.
/// macOS: (device, inode). Windows: (volume serial, file reference number).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FileId {
    pub device: u64,
    pub index: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileStatus {
    Present,
    Moved,
    Trashed,
    Archived,
    Missing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    File,
    Dir,
    Link,
    Other,
}

/// A row in the `files` inventory table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRecord {
    pub file_id: FileId,
    pub root_id: i64,
    pub path: PathBuf,
    pub name: String,
    pub ext: Option<String>,
    pub size: u64,
    pub mtime: DateTime<Utc>,
    pub ctime: Option<DateTime<Utc>>,
    pub birthtime: Option<DateTime<Utc>>,
    pub blob_hash: Option<String>,
    pub kind: EntryKind,
    pub is_link: bool,
    pub status: FileStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    Document,
    Invoice,
    Contract,
    Photo,
    Screenshot,
    Design,
    Code,
    Archive,
    Installer,
    Media,
    Data,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Classification {
    pub category: Category,
    pub confidence: f32,
    pub signals: Vec<String>,
    pub source: ClassificationSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClassificationSource {
    Rule,
    Ml,
    Llm,
    User,
}
