pub struct AgentDef {
    pub name: String,
    pub description: String,
    pub body: String,
}

pub fn agents_dir() -> Option<std::path::PathBuf> {
    workspace::git_root().map(|root| root.join(".ns2").join("agents"))
}

pub fn parse_agent_content(content: &str) -> Option<AgentDef> {
    let content = content.trim_start();
    let rest = content.strip_prefix("---\n")?;
    let (frontmatter, body) = if let Some(pos) = rest.find("\n---\n") {
        (&rest[..pos], rest[pos + 5..].trim())
    } else if let Some(stripped) = rest.strip_suffix("\n---") {
        (stripped, "")
    } else {
        return None;
    };

    let mut name = None;
    let mut description = String::new();
    for line in frontmatter.lines() {
        if let Some(v) = line.strip_prefix("name: ") {
            name = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("description: ") {
            description = v.trim().to_string();
        }
    }

    Some(AgentDef { name: name?, description, body: body.to_string() })
}

/// Format an agent definition as a frontmatter `.md` file.
/// Note: `name` and `description` are written verbatim. Newlines or colons in those
/// fields will produce malformed frontmatter. Callers (e.g. CLI flags) are responsible
/// for ensuring single-line values.
pub fn format_agent_file(def: &AgentDef) -> String {
    format!("---\nname: {}\ndescription: {}\n---\n\n{}", def.name, def.description, def.body)
}

pub fn load_agent(dir: &std::path::Path, name: &str) -> Option<AgentDef> {
    let path = dir.join(format!("{name}.md"));
    let content = std::fs::read_to_string(path).ok()?;
    parse_agent_content(&content)
}

pub fn list_agents(dir: &std::path::Path) -> Vec<AgentDef> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return vec![],
    };
    let mut agents: Vec<AgentDef> = entries
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                return None;
            }
            let content = std::fs::read_to_string(&path).ok()?;
            match parse_agent_content(&content) {
                Some(def) => Some(def),
                None => {
                    tracing::warn!("skipping {}: invalid frontmatter", path.display());
                    None
                }
            }
        })
        .collect();
    agents.sort_by(|a, b| a.name.cmp(&b.name));
    agents
}

/// Write an agent definition to `{dir}/{name}.md`.
/// Overwrites the file if it already exists. Callers are responsible for checking
/// existence before calling if overwrite should be prevented (e.g. `agent new`).
pub fn write_agent(dir: &std::path::Path, def: &AgentDef) -> std::io::Result<()> {
    std::fs::write(dir.join(format!("{}.md", def.name)), format_agent_file(def))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Tests moved from cli ─────────────────────────────────────────────────

    #[test]
    fn parse_agent_content_extracts_name_and_description() {
        let content = "---\nname: coding\ndescription: A coding agent\n---\n\nSystem prompt here.\n";
        let def = parse_agent_content(content).unwrap();
        assert_eq!(def.name, "coding");
        assert_eq!(def.description, "A coding agent");
        assert_eq!(def.body, "System prompt here.");
    }

    #[test]
    fn parse_agent_content_empty_body() {
        let content = "---\nname: coding\ndescription: minimal\n---\n";
        let def = parse_agent_content(content).unwrap();
        assert_eq!(def.name, "coding");
        assert_eq!(def.body, "");
    }

    #[test]
    fn parse_agent_content_missing_description_defaults_to_empty() {
        let content = "---\nname: coding\n---\n\nBody.\n";
        let def = parse_agent_content(content).unwrap();
        assert_eq!(def.description, "");
        assert_eq!(def.body, "Body.");
    }

    #[test]
    fn parse_agent_content_missing_frontmatter_returns_none() {
        let content = "just some plain text without frontmatter";
        assert!(parse_agent_content(content).is_none());
    }

    #[test]
    fn parse_agent_content_missing_name_returns_none() {
        let content = "---\ndescription: no name here\n---\n\nBody.\n";
        assert!(parse_agent_content(content).is_none());
    }

    #[test]
    fn format_agent_file_round_trips() {
        let def = AgentDef {
            name: "coding".to_string(),
            description: "A coding agent".to_string(),
            body: "You are a coding agent.".to_string(),
        };
        let formatted = format_agent_file(&def);
        let parsed = parse_agent_content(&formatted).unwrap();
        assert_eq!(parsed.name, def.name);
        assert_eq!(parsed.description, def.description);
        assert_eq!(parsed.body, def.body);
    }

    // ── New tests for load_agent and write_agent ─────────────────────────────

    #[test]
    fn write_agent_then_load_agent_round_trips() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();
        let def = AgentDef {
            name: "myagent".to_string(),
            description: "Does things".to_string(),
            body: "You are a helpful agent.".to_string(),
        };
        write_agent(dir, &def).unwrap();
        let loaded = load_agent(dir, "myagent").unwrap();
        assert_eq!(loaded.name, def.name);
        assert_eq!(loaded.description, def.description);
        assert_eq!(loaded.body, def.body);
    }

    #[test]
    fn load_agent_returns_none_for_missing_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(load_agent(tmp.path(), "nonexistent").is_none());
    }

    // ── Tests for list_agents ────────────────────────────────────────────────

    #[test]
    fn list_agents_returns_only_md_files_sorted_by_name() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();

        write_agent(dir, &AgentDef {
            name: "zebra".to_string(),
            description: "last".to_string(),
            body: "z body".to_string(),
        })
        .unwrap();
        write_agent(dir, &AgentDef {
            name: "alpha".to_string(),
            description: "first".to_string(),
            body: "a body".to_string(),
        })
        .unwrap();
        // Write a non-.md file that must be ignored
        std::fs::write(dir.join("ignored.txt"), "not an agent").unwrap();

        let agents = list_agents(dir);
        assert_eq!(agents.len(), 2, "expected exactly 2 agents (ignoring .txt file)");
        assert_eq!(agents[0].name, "alpha");
        assert_eq!(agents[1].name, "zebra");
    }

    #[test]
    fn list_agents_on_nonexistent_path_returns_empty_vec() {
        let tmp = tempfile::TempDir::new().unwrap();
        let missing = tmp.path().join("does_not_exist");
        let agents = list_agents(&missing);
        assert!(agents.is_empty(), "expected empty vec for nonexistent path");
    }

    // ── agents_dir() path construction ──────────────────────────────────────

    /// agents_dir() must agree with workspace::git_root().map(|r| r.join(".ns2").join("agents")).
    /// When git_root() returns None (outside a git repo, e.g. in cargo-mutants temp dir),
    /// both sides are None and the test still passes, which is fine — the mutation
    /// `agents_dir() -> None` is indistinguishable from the real implementation outside
    /// a git repo. The `Some(Default::default())` mutation IS distinguishable because
    /// git_root() returns None in the temp dir so the real impl returns None too, while
    /// the mutant would return Some(PathBuf::new()).
    #[test]
    fn agents_dir_matches_git_root_join() {
        let expected = workspace::git_root().map(|r| r.join(".ns2").join("agents"));
        let actual = agents_dir();
        assert_eq!(actual, expected, "agents_dir() must equal git_root().join('.ns2').join('agents')");
    }

    /// agents_dir() must return an absolute path when inside a git repo.
    /// Skip gracefully when not inside a git repo (e.g. cargo-mutants temp dir).
    #[test]
    fn agents_dir_returns_absolute_path() {
        let dir = match agents_dir() {
            Some(d) => d,
            None => return, // not inside a git repo; skip
        };
        assert!(dir.is_absolute(), "agents_dir() must return an absolute path, got: {}", dir.display());
    }

    /// parse_agent_content correctly extracts the body (multi-line).
    #[test]
    fn parse_agent_content_multiline_body() {
        let content = "---\nname: multi\ndescription: test\n---\n\nLine one.\nLine two.\n";
        let def = parse_agent_content(content).unwrap();
        assert_eq!(def.body, "Line one.\nLine two.");
    }

    /// format_agent_file produces a string that starts with "---\n".
    #[test]
    fn format_agent_file_starts_with_frontmatter_delimiter() {
        let def = AgentDef {
            name: "agent".to_string(),
            description: "desc".to_string(),
            body: "body text".to_string(),
        };
        let formatted = format_agent_file(&def);
        assert!(formatted.starts_with("---\n"), "formatted output must start with '---\\n'");
    }

    /// load_agent returns None for a file that exists but has invalid frontmatter.
    #[test]
    fn load_agent_returns_none_for_invalid_content() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("bad.md"), "not valid frontmatter").unwrap();
        assert!(load_agent(tmp.path(), "bad").is_none());
    }

    /// The body may contain `---` on its own line (e.g. a markdown horizontal rule).
    /// The parser finds the *first* `\n---\n` in the file, which is always the
    /// frontmatter closing delimiter; any `---` in the body is preserved verbatim.
    #[test]
    fn parse_agent_content_body_with_horizontal_rule_preserved() {
        let content = "---\nname: agent\ndescription: desc\n---\n\nSection one.\n\n---\n\nSection two.\n";
        let def = parse_agent_content(content).unwrap();
        assert_eq!(def.name, "agent");
        assert_eq!(def.body, "Section one.\n\n---\n\nSection two.");
    }

    /// list_agents silently skips `.md` files whose content cannot be parsed
    /// (no valid frontmatter). Only well-formed agent files are returned.
    #[test]
    fn list_agents_skips_invalid_frontmatter_md_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();

        write_agent(dir, &AgentDef {
            name: "good".to_string(),
            description: "valid".to_string(),
            body: "body".to_string(),
        })
        .unwrap();
        std::fs::write(dir.join("bad.md"), "not valid frontmatter at all").unwrap();

        let agents = list_agents(dir);
        assert_eq!(agents.len(), 1, "invalid .md file must be silently skipped");
        assert_eq!(agents[0].name, "good");
    }

    /// CRLF line endings are not supported: `parse_agent_content` requires LF-only.
    /// Files with Windows line endings will fail to parse and return `None`.
    #[test]
    fn parse_agent_content_crlf_returns_none() {
        let content = "---\r\nname: agent\r\ndescription: desc\r\n---\r\n\r\nBody.\r\n";
        assert!(
            parse_agent_content(content).is_none(),
            "CRLF line endings are not supported and must return None"
        );
    }

    /// write_agent creates a file named `{name}.md` in the given directory.
    #[test]
    fn write_agent_creates_correct_filename() {
        let tmp = tempfile::TempDir::new().unwrap();
        let def = AgentDef {
            name: "mybot".to_string(),
            description: "desc".to_string(),
            body: "body".to_string(),
        };
        write_agent(tmp.path(), &def).unwrap();
        assert!(
            tmp.path().join("mybot.md").exists(),
            "write_agent must create {{name}}.md"
        );
    }
}
