use anyhow::{Result, bail};
use regex::Regex;
use std::path::Path;

pub const MAX_TEXT_BYTES: u64 = 1024 * 1024;
pub const MAX_TOTAL_TEXT_BYTES: u64 = 10 * 1024 * 1024;
pub const MAX_FILES: usize = 512;
pub const MAX_DEPTH: usize = 8;

pub fn safe_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

pub fn safe_environment_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().enumerate().all(|(index, byte)| {
            (byte == b'_' || byte.is_ascii_alphabetic()) && index == 0
                || (byte == b'_' || byte.is_ascii_alphanumeric()) && index > 0
        })
}

pub fn checked_text(path: &str, bytes: Vec<u8>) -> Result<String> {
    if bytes.len() as u64 > MAX_TEXT_BYTES {
        bail!("Portable input exceeds the per-file size limit at {path}");
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| anyhow::anyhow!("Portable input must be UTF-8 text at {path}"))?;
    reject_secret(path, &text)?;
    Ok(text)
}

pub fn reject_secret(path: &str, value: &str) -> Result<()> {
    // This is a conservative guard for accidental disclosure, not a claim that all secrets are detectable.
    let assignment = Regex::new(
        r#"(?i)(api[_-]?key|access[_-]?token|auth[_-]?token|token|secret|password|private[_-]?key)\s*[=:]\s*['\"]?[^\s'\"]{8,}"#,
    )?;
    let json_assignment = Regex::new(
        r#"(?i)\"(?:api[_-]?key|access[_-]?token|auth[_-]?token|token|secret|password|private[_-]?key)\"\s*:\s*\"[^\"]{8,}\""#,
    )?;
    let bearer = Regex::new(r#"(?i)authorization\s*[=:]\s*['\"]?bearer\s+[^\s'\"]+"#)?;
    let provider_key = Regex::new(
        r"\b(?:sk|rk|ghp|github_pat)_[A-Za-z0-9_-]{16,}\b|\bsk-[A-Za-z0-9_-]{16,}\b|\bAKIA[0-9A-Z]{16}\b",
    )?;
    let pem = Regex::new(r"-----BEGIN (?:[A-Z ]*PRIVATE KEY|OPENSSH PRIVATE KEY)-----")?;
    if assignment.is_match(value)
        || json_assignment.is_match(value)
        || bearer.is_match(value)
        || provider_key.is_match(value)
        || pem.is_match(value)
    {
        bail!("Potential credential detected at {path}; remove it before sharing");
    }
    Ok(())
}

pub fn display_relative(path: &Path) -> Result<String> {
    let text = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("Portable path is not valid UTF-8"))?;
    if text.is_empty()
        || path.is_absolute()
        || text.bytes().any(|byte| byte.is_ascii_control())
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        bail!("Portable paths must be relative and may not traverse directories");
    }
    Ok(text.replace('\\', "/"))
}
