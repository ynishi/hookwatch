//! Masking credentials before they stay in the ledger. Finding them is left
//! to gitleaks (`gitleaks stdin`); hookwatch only replaces the exact strings
//! it reports, so the stored JSON keeps its shape.

use serde::Deserialize;

pub const MARK: &str = "[REDACTED by hookwatch]";

#[derive(Deserialize)]
struct Finding {
    #[serde(rename = "Secret")]
    secret: String,
}

/// The secrets gitleaks finds in `texts`, in one run (a run costs ~60 ms,
/// measured 2026-11-14). A missing gitleaks or a failed run is an error:
/// the caller must not take "no findings" from a scan that did not happen.
pub fn find_secrets(texts: &[&str]) -> anyhow::Result<Vec<String>> {
    use anyhow::Context;
    use std::io::Write;
    use std::process::{Command, Stdio};
    if texts.is_empty() {
        return Ok(vec![]);
    }
    let mut child = Command::new("gitleaks")
        .args([
            "stdin",
            "--no-banner",
            "--log-level",
            "error",
            "--exit-code",
            "0",
        ])
        .args(["--report-format", "json", "--report-path", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("start gitleaks (is it installed?)")?;
    let mut stdin = child.stdin.take().context("gitleaks has no stdin")?;
    let input = texts.join("\n");
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let out = child.wait_with_output().context("wait for gitleaks")?;
    match writer.join() {
        Ok(r) => r.context("write to gitleaks")?,
        Err(_) => anyhow::bail!("gitleaks stdin writer thread panicked"),
    }
    if !out.status.success() {
        anyhow::bail!(
            "gitleaks failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let found: Vec<Finding> =
        serde_json::from_slice(&out.stdout).context("gitleaks report is not JSON")?;
    let mut secrets: Vec<String> = found
        .into_iter()
        .map(|f| f.secret)
        .filter(|s| !s.is_empty())
        .collect();
    // Longest first, so a secret that contains another is replaced whole.
    secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
    secrets.dedup();
    Ok(secrets)
}

/// Replace every occurrence of every secret. Returns the text and how many
/// were replaced. Pure.
pub fn mask(text: &str, secrets: &[String]) -> (String, usize) {
    let mut out = text.to_owned();
    let mut n = 0;
    for s in secrets {
        let c = out.matches(s.as_str()).count();
        if c > 0 {
            n += c;
            out = out.replace(s.as_str(), MARK);
        }
    }
    (out, n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_replaces_exact_strings_and_keeps_json() {
        // Stand-ins: the finding is gitleaks' job, not this test's.
        let secrets = vec!["s3cr3t-long-value".to_owned(), "s3cr3t".to_owned()];
        let text = r#"{"a":"x s3cr3t-long-value y","b":"s3cr3t"}"#;
        let (out, n) = mask(text, &secrets);
        assert_eq!(n, 2);
        assert_eq!(out, format!(r#"{{"a":"x {MARK} y","b":"{MARK}"}}"#));
        assert!(serde_json::from_str::<serde_json::Value>(&out).is_ok());
        assert_eq!(mask("plain", &secrets), ("plain".to_owned(), 0));
    }
}
