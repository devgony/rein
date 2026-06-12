use anyhow::{Context, Result};
use chrono::{Local, SecondsFormat};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;

/// Write a file atomically: temp file in the same directory + rename().
pub fn atomic_write(path: &Path, contents: &str) -> Result<()> {
    let dir = path.parent().context("path has no parent")?;
    fs::create_dir_all(dir)?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .context("path has no file name")?;
    let tmp = dir.join(format!(".{}.tmp-{}", name, std::process::id()));
    fs::write(&tmp, contents)?;
    fs::rename(&tmp, path).with_context(|| format!("atomic rename to {}", path.display()))?;
    Ok(())
}

/// Lowercase slug: alnum preserved, runs of anything else collapsed to '-'.
pub fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut dash = true; // suppress leading dash
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            dash = false;
        } else if !dash {
            out.push('-');
            dash = true;
        }
    }
    let trimmed = out.trim_end_matches('-').to_string();
    if trimmed.is_empty() {
        "task".to_string()
    } else {
        trimmed
    }
}

pub fn now_iso() -> String {
    Local::now().to_rfc3339_opts(SecondsFormat::Secs, false)
}

pub fn today_compact() -> String {
    Local::now().format("%Y%m%d").to_string()
}

pub fn month_dir() -> String {
    Local::now().format("%Y-%m").to_string()
}

pub fn sha256_tag(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    let digest = h.finalize();
    let mut hex = String::with_capacity(64);
    for b in digest {
        hex.push_str(&format!("{:02x}", b));
    }
    format!("sha256:{}", hex)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("Settings Cleanup!"), "settings-cleanup");
        assert_eq!(slugify("  a  b  "), "a-b");
        assert_eq!(slugify("한글 제목"), "task");
    }
}
