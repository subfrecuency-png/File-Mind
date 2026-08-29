//! Spotlight integration via the `mdls` / `mdfind` command-line tools.
//! On non-macOS Unix these return empty results so the workspace still tests.

use filemind_core::adapter::NativeMeta;
use filemind_core::Result;
use std::path::{Path, PathBuf};
use std::process::Command;

const ATTRS: [&str; 6] = [
    "kMDItemContentType",
    "kMDItemWhereFroms",
    "kMDItemUserTags",
    "kMDItemKind",
    "kMDItemLastUsedDate",
    "kMDItemUseCount",
];

/// Parse `mdls -name ... -raw -nullMarker "" ` style output. We call `mdls`
/// without `-raw` so each attribute prints as `key = value`, which is easier
/// to split when values are arrays.
pub fn parse_mdls(text: &str) -> NativeMeta {
    let mut meta = NativeMeta::default();
    let mut current: Option<String> = None;
    let mut buf: Vec<String> = Vec::new();

    let flush = |key: &Option<String>, buf: &mut Vec<String>, meta: &mut NativeMeta| {
        let Some(k) = key else { return };
        let vals: Vec<String> = buf
            .drain(..)
            .map(|v| v.trim().trim_matches(',').trim_matches('"').to_string())
            .filter(|v| !v.is_empty() && v != "(null)")
            .collect();
        match k.as_str() {
            "kMDItemContentType" => meta.content_type = vals.first().cloned(),
            "kMDItemWhereFroms" => meta.where_from = vals,
            "kMDItemUserTags" => meta.tags = vals,
            other => {
                if let Some(v) = vals.first() {
                    meta.extra.push((other.to_string(), v.clone()));
                }
            }
        }
    };

    for line in text.lines() {
        if let Some((k, v)) = line.split_once(" = ") {
            if k.starts_with("kMDItem") {
                flush(&current, &mut buf, &mut meta);
                current = Some(k.trim().to_string());
                let v = v.trim();
                if v == "(" {
                    continue; // array follows
                }
                buf.push(v.to_string());
                continue;
            }
        }
        if line.trim() == ")" {
            continue;
        }
        if current.is_some() {
            buf.push(line.to_string());
        }
    }
    flush(&current, &mut buf, &mut meta);
    meta
}

pub fn mdls(path: &Path) -> Result<NativeMeta> {
    if !cfg!(target_os = "macos") {
        return Ok(NativeMeta::default());
    }
    let mut cmd = Command::new("mdls");
    for a in ATTRS {
        cmd.arg("-name").arg(a);
    }
    let out = cmd.arg(path).output()?;
    if !out.status.success() {
        return Ok(NativeMeta::default());
    }
    Ok(parse_mdls(&String::from_utf8_lossy(&out.stdout)))
}

pub fn mdfind(query: &str) -> Result<Vec<PathBuf>> {
    if !cfg!(target_os = "macos") {
        return Ok(Vec::new());
    }
    let out = Command::new("mdfind").arg(query).output()?;
    if !out.status.success() {
        return Ok(Vec::new());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scalar_and_array_attributes() {
        let text = r#"kMDItemContentType = "com.adobe.pdf"
kMDItemKind        = "PDF document"
kMDItemUserTags    = (
    "Client",
    "2025"
)
kMDItemWhereFroms  = (
    "https://example.com/offer.pdf"
)
kMDItemUseCount    = (null)
"#;
        let m = parse_mdls(text);
        assert_eq!(m.content_type.as_deref(), Some("com.adobe.pdf"));
        assert_eq!(m.tags, vec!["Client", "2025"]);
        assert_eq!(m.where_from, vec!["https://example.com/offer.pdf"]);
        assert!(m
            .extra
            .iter()
            .any(|(k, v)| k == "kMDItemKind" && v == "PDF document"));
        assert!(!m.extra.iter().any(|(k, _)| k == "kMDItemUseCount"));
    }
}
