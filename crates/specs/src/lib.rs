use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};

pub struct SpecDef {
    pub targets: Vec<String>,
    pub verified: Option<DateTime<Utc>>,
    pub body: String,
}

pub fn parse_spec_content(content: &str) -> Option<SpecDef> {
    let rest = content.strip_prefix("---\n")?;
    let (frontmatter, body) = if let Some(pos) = rest.find("\n---\n") {
        (&rest[..pos], rest[pos + 5..].trim())
    } else if let Some(stripped) = rest.strip_suffix("\n---") {
        (stripped, "")
    } else {
        return None;
    };

    let mut targets: Vec<String> = Vec::new();
    let mut verified: Option<DateTime<Utc>> = None;
    let mut in_targets = false;

    for line in frontmatter.lines() {
        if line == "targets:" {
            in_targets = true;
        } else if in_targets && line.starts_with("  - ") {
            targets.push(line[4..].trim().to_string());
        } else if let Some(v) = line.strip_prefix("verified: ") {
            in_targets = false;
            let dt = DateTime::parse_from_rfc3339(v.trim()).ok()?.with_timezone(&Utc);
            verified = Some(dt);
        } else {
            in_targets = false;
        }
    }

    if targets.is_empty() {
        return None;
    }

    Some(SpecDef { targets, verified, body: body.to_string() })
}

pub fn format_spec_file(def: &SpecDef) -> String {
    let mut out = String::from("---\ntargets:\n");
    for t in &def.targets {
        out.push_str(&format!("  - {t}\n"));
    }
    if let Some(v) = def.verified {
        out.push_str(&format!("verified: {}\n", v.format("%Y-%m-%dT%H:%M:%SZ")));
    }
    out.push_str("---\n");
    if !def.body.is_empty() {
        out.push_str(&format!("\n\n{}", def.body));
    }
    out
}

pub fn load_spec(path: &Path) -> Option<SpecDef> {
    let content = std::fs::read_to_string(path).ok()?;
    parse_spec_content(&content)
}

pub fn write_spec(path: &Path, def: &SpecDef) -> std::io::Result<()> {
    std::fs::write(path, format_spec_file(def))
}

pub fn list_specs(root: &Path) -> Vec<(PathBuf, SpecDef)> {
    let pattern = format!("{}/**/*.spec.md", root.display());
    let mut results: Vec<(PathBuf, SpecDef)> = glob::glob(&pattern)
        .unwrap_or_else(|_| glob::glob("").unwrap())
        .filter_map(|entry| {
            let path = entry.ok()?;
            let content = std::fs::read_to_string(&path).ok()?;
            match parse_spec_content(&content) {
                Some(def) => Some((path, def)),
                None => {
                    tracing::warn!("skipping {}: invalid frontmatter", path.display());
                    None
                }
            }
        })
        .collect();
    results.sort_by(|a, b| a.0.cmp(&b.0));
    results
}

pub fn stale_files(root: &Path, def: &SpecDef) -> Vec<PathBuf> {
    let mut matched: Vec<PathBuf> = Vec::new();
    for target in &def.targets {
        let pattern = format!("{}/{}", root.display(), target);
        let entries = match glob::glob(&pattern) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.filter_map(|e| e.ok()) {
            let meta = match std::fs::metadata(&entry) {
                Ok(m) => m,
                Err(_) => continue,
            };
            let modified = match meta.modified() {
                Ok(t) => t,
                Err(_) => continue,
            };
            let modified_dt: DateTime<Utc> = modified.into();
            match def.verified {
                None => matched.push(entry),
                Some(v) if modified_dt > v => matched.push(entry),
                _ => {}
            }
        }
    }
    matched.sort();
    matched.dedup();
    matched
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    const SAMPLE: &str = "---\ntargets:\n  - crates/cli/src/**/*.rs\n  - crates/agents/Cargo.toml\nverified: 2024-01-15T10:30:00Z\n---\n\nOptional body text here.\n";

    #[test]
    fn parse_spec_content_extracts_targets() {
        let def = parse_spec_content(SAMPLE).unwrap();
        assert_eq!(def.targets, vec!["crates/cli/src/**/*.rs", "crates/agents/Cargo.toml"]);
    }

    #[test]
    fn parse_spec_content_extracts_verified() {
        let def = parse_spec_content(SAMPLE).unwrap();
        let expected = chrono::DateTime::parse_from_rfc3339("2024-01-15T10:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(def.verified, Some(expected));
    }

    #[test]
    fn parse_spec_content_missing_verified_is_none_field() {
        let content = "---\ntargets:\n  - crates/cli/src/**/*.rs\n---\n\nBody.\n";
        let def = parse_spec_content(content).unwrap();
        assert_eq!(def.verified, None);
    }

    #[test]
    fn parse_spec_content_returns_none_without_targets() {
        let content = "---\nverified: 2024-01-15T10:30:00Z\n---\n\nBody.\n";
        assert!(parse_spec_content(content).is_none());
    }

    #[test]
    fn parse_spec_content_returns_none_for_empty_targets() {
        let content = "---\ntargets:\nverified: 2024-01-15T10:30:00Z\n---\n\nBody.\n";
        assert!(parse_spec_content(content).is_none());
    }

    #[test]
    fn parse_spec_content_returns_none_invalid_frontmatter() {
        let content = "just some plain text without frontmatter";
        assert!(parse_spec_content(content).is_none());
    }

    #[test]
    fn parse_spec_content_returns_none_for_invalid_timestamp() {
        let content = "---\ntargets:\n  - crates/cli/src/**/*.rs\nverified: not-a-timestamp\n---\n\nBody.\n";
        assert!(parse_spec_content(content).is_none());
    }

    #[test]
    fn parse_spec_content_extracts_body() {
        let def = parse_spec_content(SAMPLE).unwrap();
        assert_eq!(def.body, "Optional body text here.");
    }

    #[test]
    fn parse_spec_content_empty_body_ok() {
        let content = "---\ntargets:\n  - crates/cli/src/**/*.rs\n---\n";
        let def = parse_spec_content(content).unwrap();
        assert_eq!(def.body, "");
    }

    #[test]
    fn parse_spec_content_crlf_returns_none() {
        let content = "---\r\ntargets:\r\n  - crates/cli/src/**/*.rs\r\n---\r\n\r\nBody.\r\n";
        assert!(parse_spec_content(content).is_none());
    }

    #[test]
    fn format_spec_file_round_trips_with_verified() {
        let verified = chrono::DateTime::parse_from_rfc3339("2024-01-15T10:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let def = SpecDef {
            targets: vec!["crates/cli/src/**/*.rs".to_string(), "crates/agents/Cargo.toml".to_string()],
            verified: Some(verified),
            body: "Some spec body.".to_string(),
        };
        let formatted = format_spec_file(&def);
        let parsed = parse_spec_content(&formatted).unwrap();
        assert_eq!(parsed.targets, def.targets);
        assert_eq!(parsed.verified, def.verified);
        assert_eq!(parsed.body, def.body);
    }

    #[test]
    fn format_spec_file_round_trips_without_verified() {
        let def = SpecDef {
            targets: vec!["crates/cli/src/**/*.rs".to_string()],
            verified: None,
            body: "Body text.".to_string(),
        };
        let formatted = format_spec_file(&def);
        let parsed = parse_spec_content(&formatted).unwrap();
        assert_eq!(parsed.targets, def.targets);
        assert_eq!(parsed.verified, None);
        assert_eq!(parsed.body, def.body);
    }

    #[test]
    fn format_spec_file_omits_verified_when_none() {
        let def = SpecDef {
            targets: vec!["crates/cli/src/**/*.rs".to_string()],
            verified: None,
            body: "Body.".to_string(),
        };
        let formatted = format_spec_file(&def);
        assert!(!formatted.contains("verified:"), "verified: line must be absent when verified is None");
    }

    #[test]
    fn write_spec_then_load_spec_round_trips() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("myspec.spec.md");
        let verified = chrono::DateTime::parse_from_rfc3339("2024-06-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let def = SpecDef {
            targets: vec!["crates/cli/src/**/*.rs".to_string()],
            verified: Some(verified),
            body: "Spec body.".to_string(),
        };
        write_spec(&path, &def).unwrap();
        let loaded = load_spec(&path).unwrap();
        assert_eq!(loaded.targets, def.targets);
        assert_eq!(loaded.verified, def.verified);
        assert_eq!(loaded.body, def.body);
    }

    #[test]
    fn load_spec_returns_none_for_missing_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("nonexistent.spec.md");
        assert!(load_spec(&path).is_none());
    }

    #[test]
    fn load_spec_returns_none_for_invalid_content() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("bad.spec.md");
        std::fs::write(&path, "not valid frontmatter").unwrap();
        assert!(load_spec(&path).is_none());
    }

    #[test]
    fn list_specs_finds_spec_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        let def = SpecDef {
            targets: vec!["crates/cli/src/**/*.rs".to_string()],
            verified: None,
            body: "Body.".to_string(),
        };
        write_spec(&root.join("myspec.spec.md"), &def).unwrap();
        let specs = list_specs(root);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].1.targets, def.targets);
    }

    #[test]
    fn list_specs_skips_invalid_frontmatter() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        let def = SpecDef {
            targets: vec!["crates/cli/src/**/*.rs".to_string()],
            verified: None,
            body: "Body.".to_string(),
        };
        write_spec(&root.join("good.spec.md"), &def).unwrap();
        std::fs::write(root.join("bad.spec.md"), "no frontmatter here").unwrap();
        let specs = list_specs(root);
        assert_eq!(specs.len(), 1);
    }

    #[test]
    fn list_specs_returns_empty_for_empty_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let specs = list_specs(tmp.path());
        assert!(specs.is_empty());
    }

    #[test]
    fn stale_files_returns_all_when_unverified() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("main.rs"), "fn main() {}").unwrap();
        let def = SpecDef {
            targets: vec!["*.rs".to_string()],
            verified: None,
            body: String::new(),
        };
        let stale = stale_files(root, &def);
        assert_eq!(stale.len(), 1);
    }

    #[test]
    fn stale_files_returns_empty_when_all_fresh() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("main.rs"), "fn main() {}").unwrap();
        let future = Utc.timestamp_opt(9_999_999_999, 0).unwrap();
        let def = SpecDef {
            targets: vec!["*.rs".to_string()],
            verified: Some(future),
            body: String::new(),
        };
        let stale = stale_files(root, &def);
        assert!(stale.is_empty());
    }

    #[test]
    fn stale_files_returns_modified_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("main.rs"), "fn main() {}").unwrap();
        let epoch = Utc.timestamp_opt(0, 0).unwrap();
        let def = SpecDef {
            targets: vec!["*.rs".to_string()],
            verified: Some(epoch),
            body: String::new(),
        };
        let stale = stale_files(root, &def);
        assert_eq!(stale.len(), 1);
    }
}
