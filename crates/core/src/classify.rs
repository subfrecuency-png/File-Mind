//! Rule-based classifier (layer 1) and the sensitive-content detector.
//!
//! Signals, in order of trust: a user rule for the folder or name pattern,
//! the extension, the file name, the parent folder name, and — when text
//! was extracted — keyword votes over the first few kilobytes. Every
//! decision carries the signals that produced it so the UI can explain it
//! and a correction can be turned into a rule. The ONNX text classifier
//! (layer 2) plugs in behind the same [`Classification`] type in Phase 7.

use crate::model::{Category, Classification, ClassificationSource};
use std::path::Path;

/// A rule learned from a user correction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserRule {
    /// Everything under this folder (recursively).
    PathPrefix { prefix: String, category: Category },
    /// File name contains this lower-cased token.
    NameContains { token: String, category: Category },
    /// Extension override.
    Ext { ext: String, category: Category },
}

fn by_ext(ext: &str) -> Option<(Category, f32)> {
    use Category::*;
    Some(match ext {
        "jpg" | "jpeg" | "heic" | "heif" | "png" | "gif" | "webp" | "tif" | "tiff" | "bmp"
        | "raw" | "cr2" | "nef" | "dng" | "arw" => (Photo, 0.8),
        "psd" | "ai" | "sketch" | "fig" | "xd" | "indd" | "afdesign" | "afphoto" | "svg"
        | "eps" | "lbrn2" | "blend" | "c4d" | "obj" | "fbx" | "stl" | "glb" | "gltf" | "uasset"
        | "aep" | "prproj" | "drp" => (Design, 0.9),
        "mp4" | "mov" | "m4v" | "avi" | "mkv" | "webm" | "wmv" | "flv" | "mp3" | "m4a" | "wav"
        | "aiff" | "flac" | "ogg" | "aac" => (Media, 0.95),
        "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "7z" | "rar" => (Archive, 0.95),
        "dmg" | "pkg" | "msi" | "exe" | "app" | "apk" | "appimage" | "deb" | "rpm" | "iso" => {
            (Installer, 0.95)
        }
        "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "go" | "java" | "c" | "cpp" | "h" | "hpp"
        | "swift" | "kt" | "rb" | "php" | "sh" | "zsh" | "bash" | "sql" | "ipynb" | "toml"
        | "yaml" | "yml" | "lock" | "css" | "scss" | "html" | "htm" | "vue" | "svelte" => {
            (Code, 0.85)
        }
        "csv" | "tsv" | "json" | "xml" | "parquet" | "db" | "sqlite" | "xlsx" | "xls"
        | "numbers" => (Data, 0.7),
        "pdf" | "doc" | "docx" | "pages" | "rtf" | "txt" | "md" | "odt" | "ppt" | "pptx"
        | "key" | "epub" => (Document, 0.55),
        _ => return None,
    })
}

/// Keyword votes for document sub-types. Weights are per hit, capped.
const INVOICE_WORDS: &[&str] = &[
    "invoice",
    "receipt",
    "amount due",
    "total due",
    "bill to",
    "payment",
    "paid",
    "balance due",
    "tax id",
    "qty",
];
const CONTRACT_WORDS: &[&str] = &[
    "agreement",
    "contract",
    "hereby",
    "whereas",
    "parties",
    "terms and conditions",
    "indemnif",
    "governing law",
    "signature",
    "offer sheet",
    "lease",
    "nda",
    "non-disclosure",
];
const SCREENSHOT_WORDS: &[&str] = &["screenshot", "screen shot", "capture", "cleanshot", "snip"];

fn count_hits(hay: &str, words: &[&str]) -> usize {
    words.iter().filter(|w| hay.contains(*w)).count()
}

/// Classify one file from its path, an optional parent-folder hint, an
/// optional text excerpt, and the user's rules (checked first).
pub fn classify(path: &Path, text: Option<&str>, rules: &[UserRule]) -> Classification {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let parent = path
        .parent()
        .map(|p| p.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let mut signals: Vec<String> = Vec::new();

    // 1. user rules
    for r in rules {
        match r {
            UserRule::PathPrefix { prefix, category } if path.starts_with(prefix) => {
                return Classification {
                    category: *category,
                    confidence: 0.99,
                    signals: vec![format!("user rule: folder {prefix}")],
                    source: ClassificationSource::User,
                };
            }
            UserRule::NameContains { token, category } if name.contains(token.as_str()) => {
                return Classification {
                    category: *category,
                    confidence: 0.97,
                    signals: vec![format!("user rule: name contains '{token}'")],
                    source: ClassificationSource::User,
                };
            }
            UserRule::Ext { ext: e, category } if *e == ext => {
                return Classification {
                    category: *category,
                    confidence: 0.95,
                    signals: vec![format!("user rule: .{e}")],
                    source: ClassificationSource::User,
                };
            }
            _ => {}
        }
    }

    // 2. extension
    let (mut category, mut confidence) = match by_ext(&ext) {
        Some((c, conf)) => {
            signals.push(format!("extension .{ext}"));
            (c, conf)
        }
        None => (Category::Other, 0.2),
    };

    // 3. name and folder refinements
    if matches!(category, Category::Photo) && count_hits(&name, SCREENSHOT_WORDS) > 0 {
        category = Category::Screenshot;
        confidence = 0.95;
        signals.push("name says screenshot".into());
    } else if matches!(category, Category::Photo)
        && (name.starts_with("img_") || name.starts_with("dsc_") || name.starts_with("pxl_"))
    {
        confidence = 0.9;
        signals.push("camera file name".into());
    }
    if matches!(
        category,
        Category::Document | Category::Data | Category::Other
    ) {
        let inv = count_hits(&name, INVOICE_WORDS);
        let con = count_hits(&name, CONTRACT_WORDS);
        if inv > 0 && inv >= con {
            category = Category::Invoice;
            confidence = 0.8;
            signals.push("name mentions invoice/receipt".into());
        } else if con > 0 {
            category = Category::Contract;
            confidence = 0.8;
            signals.push("name mentions agreement/contract".into());
        }
    }
    if matches!(
        category,
        Category::Document | Category::Data | Category::Other
    ) {
        for (folder, cat) in [
            ("invoice", Category::Invoice),
            ("receipt", Category::Invoice),
            ("contract", Category::Contract),
            ("agreement", Category::Contract),
            ("legal", Category::Contract),
            ("screenshot", Category::Screenshot),
            ("design", Category::Design),
        ] {
            if parent.contains(folder) {
                category = cat;
                confidence = confidence.max(0.7);
                signals.push(format!("folder mentions {folder}"));
                break;
            }
        }
    }
    if matches!(category, Category::Other)
        && (parent.contains("node_modules") || parent.contains("/.git/"))
    {
        category = Category::Code;
        confidence = 0.9;
        signals.push("inside a code tree".into());
    }

    // 4. text votes (documents only; we never reclassify a photo from words)
    if let Some(t) = text {
        if matches!(
            category,
            Category::Document
                | Category::Data
                | Category::Invoice
                | Category::Contract
                | Category::Other
        ) {
            let head: String = t.chars().take(6000).collect::<String>().to_lowercase();
            let inv = count_hits(&head, INVOICE_WORDS);
            let con = count_hits(&head, CONTRACT_WORDS);
            if inv >= 3 && inv > con {
                if category != Category::Invoice {
                    signals.push(format!("text: {inv} invoice terms"));
                }
                category = Category::Invoice;
                confidence = confidence.max(0.85);
            } else if con >= 3 {
                if category != Category::Contract {
                    signals.push(format!("text: {con} contract terms"));
                }
                category = Category::Contract;
                confidence = confidence.max(0.85);
            } else if category == Category::Document {
                confidence = confidence.max(0.7);
                signals.push("text extracted".into());
            } else if category == Category::Other && !head.trim().is_empty() {
                category = Category::Document;
                confidence = 0.6;
                signals.push("readable text, no known extension".into());
            }
        }
    }

    Classification {
        category,
        confidence,
        signals,
        source: ClassificationSource::Rule,
    }
}

// ----- sensitive content ---------------------------------------------------

/// Why a file was marked sensitive. Kept coarse on purpose: the value is
/// never stored, only the kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sensitive {
    SecretsFile,
    PrivateKey,
    ApiKey,
    CardNumber,
    NationalId,
}

const SECRET_NAMES: &[&str] = &[
    ".env",
    ".netrc",
    ".npmrc",
    ".pypirc",
    "id_rsa",
    "id_ed25519",
    ".htpasswd",
    "credentials",
    "secrets",
    ".pgpass",
    "keychain",
];

pub fn sensitive_by_name(path: &Path) -> Option<Sensitive> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if SECRET_NAMES
        .iter()
        .any(|s| name == *s || name.starts_with(&format!("{s}.")) || name.ends_with(s))
    {
        return Some(Sensitive::SecretsFile);
    }
    if [
        "pem", "key", "p12", "pfx", "keystore", "jks", "asc", "gpg", "ppk",
    ]
    .contains(&ext.as_str())
    {
        return Some(Sensitive::PrivateKey);
    }
    None
}

/// Scan an excerpt of text for secrets and identifiers.
pub fn sensitive_by_text(text: &str) -> Option<Sensitive> {
    let t: &str = &text[..text.len().min(64 * 1024)];
    if t.contains("-----BEGIN") && t.contains("PRIVATE KEY") {
        return Some(Sensitive::PrivateKey);
    }
    for marker in [
        "AKIA",
        "sk-",
        "ghp_",
        "gho_",
        "xoxb-",
        "xoxp-",
        "AIza",
        "sk_live_",
        "rk_live_",
        "-----BEGIN PGP",
    ] {
        if let Some(i) = t.find(marker) {
            let tail: String = t[i..]
                .chars()
                .take(48)
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .collect();
            if tail.len() >= 16 {
                return Some(Sensitive::ApiKey);
            }
        }
    }
    let lower = t.to_lowercase();
    for kw in [
        "password=",
        "password:",
        "passwd=",
        "secret=",
        "api_key=",
        "apikey=",
        "token=",
    ] {
        if lower.contains(kw) {
            return Some(Sensitive::ApiKey);
        }
    }
    if has_card_number(t) {
        return Some(Sensitive::CardNumber);
    }
    if has_ssn(t) {
        return Some(Sensitive::NationalId);
    }
    None
}

fn luhn_ok(digits: &[u8]) -> bool {
    let mut sum = 0;
    let mut double = false;
    for &d in digits.iter().rev() {
        let mut v = (d - b'0') as u32;
        if double {
            v *= 2;
            if v > 9 {
                v -= 9;
            }
        }
        sum += v;
        double = !double;
    }
    sum % 10 == 0
}

/// 13–19 digit runs (spaces/dashes allowed) that pass Luhn. Requires ≥ 2 hits
/// or a "card" keyword nearby to avoid flagging one lucky invoice number.
fn has_card_number(t: &str) -> bool {
    let bytes = t.as_bytes();
    let mut hits = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let mut digits = Vec::new();
            let mut j = i;
            while j < bytes.len()
                && (bytes[j].is_ascii_digit() || bytes[j] == b' ' || bytes[j] == b'-')
                && digits.len() < 20
            {
                if bytes[j].is_ascii_digit() {
                    digits.push(bytes[j]);
                }
                j += 1;
            }
            if (13..=19).contains(&digits.len()) && luhn_ok(&digits) {
                hits += 1;
                let ctx = &t[i.saturating_sub(40)..i].to_lowercase();
                if ctx.contains("card")
                    || ctx.contains("visa")
                    || ctx.contains("mastercard")
                    || ctx.contains("amex")
                {
                    return true;
                }
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    hits >= 2
}

/// US SSN pattern `ddd-dd-dddd` with an "ssn" / "social security" cue nearby.
fn has_ssn(t: &str) -> bool {
    let lower = t.to_lowercase();
    if !(lower.contains("ssn") || lower.contains("social security")) {
        return false;
    }
    let b = lower.as_bytes();
    for i in 0..b.len().saturating_sub(10) {
        let w = &b[i..i + 11];
        let ok = w[3] == b'-'
            && w[6] == b'-'
            && w.iter().enumerate().all(|(k, c)| {
                if k == 3 || k == 6 {
                    true
                } else {
                    c.is_ascii_digit()
                }
            })
            && (i == 0 || !b[i - 1].is_ascii_digit())
            && (i + 11 == b.len() || !b[i + 11].is_ascii_digit());
        if ok {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use Category::*;

    #[test]
    fn extension_and_name_rules() {
        let cases: &[(&str, Category)] = &[
            ("/u/Downloads/OFFER SHEET Calcium.pdf", Contract),
            ("/u/Downloads/Invoice_1042.pdf", Invoice),
            ("/u/Downloads/receipt-amazon.pdf", Invoice),
            (
                "/u/Desktop/Screenshot 2026-08-29 at 1.02.03 PM.png",
                Screenshot,
            ),
            ("/u/Pictures/IMG_4021.HEIC", Photo),
            ("/u/Projects/site/src/main.rs", Code),
            ("/u/Downloads/node_modules/x/y.z", Code),
            ("/u/Downloads/app-1.2.dmg", Installer),
            ("/u/Downloads/archive.zip", Archive),
            ("/u/Movies/clip.mp4", Media),
            ("/u/Documents/notes.md", Document),
            ("/u/Documents/data.csv", Data),
            ("/u/Design/logo.ai", Design),
            ("/u/Documents/laser.lbrn2", Design),
            ("/u/Documents/Contracts/2025 lease.pdf", Contract),
            ("/u/Documents/mystery.xyz", Other),
        ];
        for (p, want) in cases {
            let c = classify(Path::new(p), None, &[]);
            assert_eq!(c.category, *want, "{p}: {:?}", c.signals);
        }
    }

    #[test]
    fn text_votes_and_user_rules() {
        let inv = classify(
            Path::new("/u/Documents/scan_0042.pdf"),
            Some(
                "INVOICE #0042  Bill to: Acme  Amount due: $400  Payment terms: net 30  Total due",
            ),
            &[],
        );
        assert_eq!(inv.category, Invoice, "{:?}", inv.signals);
        let rules = [UserRule::PathPrefix {
            prefix: "/u/Documents/Clients".into(),
            category: Contract,
        }];
        let c = classify(Path::new("/u/Documents/Clients/x/scan.pdf"), None, &rules);
        assert_eq!(
            (c.category, c.source),
            (Contract, ClassificationSource::User)
        );
    }

    #[test]
    fn sensitive_detection() {
        assert_eq!(
            sensitive_by_name(Path::new("/p/.env")),
            Some(Sensitive::SecretsFile)
        );
        assert_eq!(
            sensitive_by_name(Path::new("/p/.env.local")),
            Some(Sensitive::SecretsFile)
        );
        assert_eq!(
            sensitive_by_name(Path::new("/p/server.pem")),
            Some(Sensitive::PrivateKey)
        );
        assert_eq!(sensitive_by_name(Path::new("/p/notes.txt")), None);
        assert_eq!(
            sensitive_by_text("-----BEGIN RSA PRIVATE KEY-----\nMIIE"),
            Some(Sensitive::PrivateKey)
        );
        assert_eq!(
            sensitive_by_text("AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE"),
            Some(Sensitive::ApiKey)
        );
        assert_eq!(
            sensitive_by_text("card: 4111 1111 1111 1111"),
            Some(Sensitive::CardNumber)
        );
        assert_eq!(
            sensitive_by_text("SSN 123-45-6789"),
            Some(Sensitive::NationalId)
        );
        assert_eq!(
            sensitive_by_text("Invoice number 4111111111111111 for parts"),
            None,
            "one Luhn hit without a cue is not enough"
        );
        assert_eq!(
            sensitive_by_text("just an ordinary letter about 2025 plans"),
            None
        );
    }
}
