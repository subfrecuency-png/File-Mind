//! Text extraction for the lexical index and the classifier.
//!
//! Only formats we can read without a heavyweight dependency: plain text and
//! code, Markdown, CSV, PDF (via `pdf-extract`), and DOCX/PPTX/XLSX (XML
//! inside a zip, tags stripped). Everything is capped at [`MAX_BYTES`] input
//! and [`MAX_TEXT`] output so a stray 4 GB log cannot stall the agent.

use std::io::Read;
use std::path::Path;

pub const MAX_BYTES: u64 = 5 * 1024 * 1024;
pub const MAX_TEXT: usize = 200 * 1024;

const TEXT_EXTS: &[&str] = &[
    "txt",
    "md",
    "markdown",
    "rst",
    "csv",
    "tsv",
    "json",
    "xml",
    "yaml",
    "yml",
    "toml",
    "ini",
    "cfg",
    "conf",
    "log",
    "html",
    "htm",
    "css",
    "js",
    "ts",
    "tsx",
    "jsx",
    "py",
    "rs",
    "go",
    "java",
    "c",
    "cpp",
    "h",
    "hpp",
    "swift",
    "kt",
    "rb",
    "php",
    "sh",
    "zsh",
    "bash",
    "sql",
    "env",
    "gitignore",
    "eml",
    "ics",
    "vcf",
    "srt",
    "vtt",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Extractor {
    Text,
    Pdf,
    OfficeXml,
    None,
}

pub fn extractor_for(path: &Path) -> Extractor {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "pdf" => Extractor::Pdf,
        "docx" | "pptx" | "xlsx" => Extractor::OfficeXml,
        e if TEXT_EXTS.contains(&e) => Extractor::Text,
        "" => {
            // extension-less files that are probably text (README, Makefile, LICENSE)
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_uppercase())
                .unwrap_or_default();
            if [
                "README",
                "LICENSE",
                "MAKEFILE",
                "DOCKERFILE",
                "CHANGELOG",
                "NOTES",
            ]
            .iter()
            .any(|n| name.starts_with(n))
            {
                Extractor::Text
            } else {
                Extractor::None
            }
        }
        _ => Extractor::None,
    }
}

/// Extract text, or `Ok(None)` when the file type is unsupported or too large.
pub fn extract(path: &Path, size: u64) -> std::io::Result<Option<String>> {
    if size > MAX_BYTES {
        return Ok(None);
    }
    let text = match extractor_for(path) {
        Extractor::None => return Ok(None),
        Extractor::Text => {
            let f = std::fs::File::open(path)?;
            let mut buf = Vec::with_capacity(size.min(MAX_BYTES) as usize);
            f.take(MAX_BYTES).read_to_end(&mut buf)?;
            if looks_binary(&buf) {
                return Ok(None);
            }
            String::from_utf8_lossy(&buf).into_owned()
        }
        Extractor::Pdf => {
            let bytes = std::fs::read(path)?;
            match std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem(&bytes)) {
                Ok(Ok(t)) => t,
                _ => return Ok(None), // encrypted, scanned, or malformed
            }
        }
        Extractor::OfficeXml => match office_xml_text(path) {
            Ok(t) => t,
            Err(_) => return Ok(None),
        },
    };
    Ok(Some(clean(&text)))
}

fn looks_binary(buf: &[u8]) -> bool {
    let sample = &buf[..buf.len().min(8192)];
    sample.contains(&0)
        || sample
            .iter()
            .filter(|b| **b < 9 || (**b > 13 && **b < 32))
            .count()
            > sample.len() / 20
}

/// Word/PowerPoint/Excel: concatenate the text nodes of every XML part.
fn office_xml_text(path: &Path) -> anyhow::Result<String> {
    let f = std::fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(f)?;
    let mut out = String::new();
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let name = entry.name().to_string();
        let wanted = name.starts_with("word/document")
            || name.starts_with("word/header")
            || name.starts_with("word/footer")
            || name.starts_with("ppt/slides/slide")
            || name.starts_with("xl/sharedStrings")
            || name == "docProps/core.xml";
        if !wanted || entry.size() > MAX_BYTES {
            continue;
        }
        let mut xml = String::new();
        entry.read_to_string(&mut xml)?;
        strip_tags_into(&xml, &mut out);
        out.push('\n');
        if out.len() > MAX_TEXT * 2 {
            break;
        }
    }
    Ok(out)
}

fn strip_tags_into(xml: &str, out: &mut String) {
    let mut in_tag = false;
    let mut last_space = true;
    for c in xml.chars() {
        match c {
            '<' => {
                in_tag = true;
                if !last_space {
                    out.push(' ');
                    last_space = true;
                }
            }
            '>' => in_tag = false,
            _ if in_tag => {}
            c if c.is_whitespace() => {
                if !last_space {
                    out.push(' ');
                    last_space = true;
                }
            }
            c => {
                out.push(c);
                last_space = false;
            }
        }
    }
}

/// Collapse whitespace, drop control characters, cap length on a char boundary.
pub fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(MAX_TEXT));
    let mut last_space = true;
    for c in text.chars() {
        if c.is_whitespace() {
            if !last_space {
                out.push(' ');
                last_space = true;
            }
        } else if !c.is_control() {
            out.push(c);
            last_space = false;
        }
        if out.len() >= MAX_TEXT {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_and_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let t = tmp.path().join("notes.md");
        std::fs::write(&t, "# Title\n\nSome   text\twith  spacing").unwrap();
        assert_eq!(
            extract(&t, 30).unwrap().unwrap(),
            "# Title Some text with spacing"
        );
        let b = tmp.path().join("blob.txt");
        std::fs::write(&b, [0u8, 1, 2, 3, 65, 66]).unwrap();
        assert!(extract(&b, 6).unwrap().is_none());
        assert!(extract(&t, MAX_BYTES + 1).unwrap().is_none());
    }

    #[test]
    fn docx_text() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("d.docx");
        let f = std::fs::File::create(&p).unwrap();
        let mut z = zip::ZipWriter::new(f);
        let opts = zip::write::SimpleFileOptions::default();
        z.start_file("word/document.xml", opts).unwrap();
        std::io::Write::write_all(
            &mut z,
            br#"<w:document><w:body><w:p><w:r><w:t>Invoice</w:t></w:r><w:r><w:t xml:space="preserve"> #1042 due</w:t></w:r></w:p></w:body></w:document>"#,
        )
        .unwrap();
        z.finish().unwrap();
        let size = std::fs::metadata(&p).unwrap().len();
        let text = extract(&p, size).unwrap().unwrap();
        assert!(text.contains("Invoice #1042 due"), "{text:?}");
    }

    #[test]
    fn extractor_choice() {
        assert_eq!(extractor_for(Path::new("a.PDF")), Extractor::Pdf);
        assert_eq!(extractor_for(Path::new("a.pptx")), Extractor::OfficeXml);
        assert_eq!(extractor_for(Path::new("README")), Extractor::Text);
        assert_eq!(extractor_for(Path::new("a.mp4")), Extractor::None);
    }
}
