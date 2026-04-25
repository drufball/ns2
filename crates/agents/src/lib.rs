use serde::Deserialize;

// ── Hook types ───────────────────────────────────────────────────────────────

/// A single hook command with an optional timeout.
#[derive(Debug, Clone)]
pub struct HookCommand {
    pub command: String,
    /// Timeout in seconds; defaults to 60.
    pub timeout: u64,
}

/// One hook entry: an optional matcher regex (None for Stop hooks) + a list of commands.
#[derive(Debug, Clone)]
pub struct HookEntry {
    /// `None` for Stop hooks; regex string for tool hooks.
    pub matcher: Option<String>,
    pub hooks: Vec<HookCommand>,
}

/// All lifecycle hooks declared for an agent.
#[derive(Debug, Clone, Default)]
pub struct AgentHooks {
    pub pre_tool_use: Vec<HookEntry>,
    pub post_tool_use: Vec<HookEntry>,
    pub stop: Vec<HookEntry>,
}

// ── YAML deserialization helpers (private) ────────────────────────────────────

/// Raw serde shape for the `hooks:` sub-block from frontmatter.
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct RawHooks {
    #[serde(rename = "PreToolUse")]
    pre_tool_use: Vec<RawHookEntry>,
    #[serde(rename = "PostToolUse")]
    post_tool_use: Vec<RawHookEntry>,
    #[serde(rename = "Stop")]
    stop: Vec<RawHookEntry>,
}

#[derive(Debug, Deserialize)]
struct RawHookEntry {
    matcher: Option<String>,
    #[serde(default)]
    hooks: Vec<RawHookCommand>,
}

#[derive(Debug, Deserialize)]
struct RawHookCommand {
    command: String,
    #[serde(default = "default_timeout")]
    timeout: u64,
}

fn default_timeout() -> u64 {
    60
}

/// Parse a `hooks:` YAML sub-block extracted from frontmatter into `AgentHooks`.
fn parse_hooks_yaml(yaml_block: &str) -> AgentHooks {
    let raw: RawHooks = serde_yaml::from_str(yaml_block).unwrap_or_default();

    let convert_entries = |entries: Vec<RawHookEntry>| -> Vec<HookEntry> {
        entries
            .into_iter()
            .map(|e| HookEntry {
                matcher: e.matcher,
                hooks: e
                    .hooks
                    .into_iter()
                    .map(|c| HookCommand { command: c.command, timeout: c.timeout })
                    .collect(),
            })
            .collect()
    };

    AgentHooks {
        pre_tool_use: convert_entries(raw.pre_tool_use),
        post_tool_use: convert_entries(raw.post_tool_use),
        stop: convert_entries(raw.stop),
    }
}

/// Extract the indented sub-block that follows a `hooks:` key at the top level
/// of YAML frontmatter. Returns `None` if `hooks:` is not present.
///
/// Strategy: scan frontmatter lines for `hooks:` at column 0, then collect all
/// subsequent lines that start with whitespace (are indented) until we hit the
/// next top-level key (non-whitespace, non-empty line) or end of input.
/// The sub-block is returned without the `hooks:` prefix so it can be parsed
/// directly by `serde_yaml`.
fn extract_hooks_subblock(frontmatter: &str) -> Option<String> {
    let mut lines = frontmatter.lines().peekable();
    // Find `hooks:` at the top level
    while let Some(line) = lines.next() {
        if line == "hooks:" || line.starts_with("hooks: ") {
            // Collect indented lines
            let mut sub_lines: Vec<&str> = Vec::new();
            while let Some(&next) = lines.peek() {
                // A top-level key starts with a non-whitespace, non-empty character
                if !next.is_empty() && !next.starts_with(' ') && !next.starts_with('\t') {
                    break;
                }
                sub_lines.push(lines.next().unwrap());
            }
            if sub_lines.is_empty() {
                return None;
            }
            // Determine the minimum indentation so we can de-indent
            let min_indent = sub_lines
                .iter()
                .filter(|l| !l.trim().is_empty())
                .map(|l| l.len() - l.trim_start().len())
                .min()
                .unwrap_or(0);
            let dedented: Vec<&str> =
                sub_lines.iter().map(|l| if l.len() >= min_indent { &l[min_indent..] } else { l }).collect();
            return Some(dedented.join("\n"));
        }
    }
    None
}

// ── AgentDef ─────────────────────────────────────────────────────────────────

pub struct AgentDef {
    pub name: String,
    pub description: String,
    pub body: String,
    pub include_project_config: bool,
    pub hooks: AgentHooks,
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
    let mut include_project_config = false;

    for line in frontmatter.lines() {
        if let Some(v) = line.strip_prefix("name: ") {
            name = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("description: ") {
            description = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("include_project_config: ") {
            include_project_config = v.trim() == "true";
        }
    }

    // Parse hooks sub-block (if present)
    let hooks = extract_hooks_subblock(frontmatter)
        .map(|sub| parse_hooks_yaml(&sub))
        .unwrap_or_default();

    Some(AgentDef {
        name: name?,
        description,
        body: body.to_string(),
        include_project_config,
        hooks,
    })
}

/// Format an agent definition as a frontmatter `.md` file.
/// Note: `name` and `description` are written verbatim. Newlines or colons in those
/// fields will produce malformed frontmatter. Callers (e.g. CLI flags) are responsible
/// for ensuring single-line values.
pub fn format_agent_file(def: &AgentDef) -> String {
    let mut frontmatter = format!("---\nname: {}\ndescription: {}", def.name, def.description);
    if def.include_project_config {
        frontmatter.push_str("\ninclude_project_config: true");
    }

    // Serialize hooks if non-empty
    let hooks_yaml = format_hooks(&def.hooks);
    if !hooks_yaml.is_empty() {
        frontmatter.push_str("\nhooks:");
        frontmatter.push_str(&hooks_yaml);
    }

    frontmatter.push_str("\n---\n\n");
    frontmatter.push_str(&def.body);
    frontmatter
}

/// Serialize `AgentHooks` to indented YAML lines (without the `hooks:` key itself).
/// Returns an empty string if there are no hooks.
fn format_hooks(hooks: &AgentHooks) -> String {
    let mut out = String::new();

    let write_entries = |out: &mut String, event: &str, entries: &[HookEntry]| {
        if entries.is_empty() {
            return;
        }
        out.push_str(&format!("\n  {event}:"));
        for entry in entries {
            out.push_str("\n    - ");
            if let Some(ref m) = entry.matcher {
                out.push_str(&format!("matcher: \"{m}\""));
                out.push_str("\n      hooks:");
            } else {
                out.push_str("hooks:");
            }
            for cmd in &entry.hooks {
                out.push_str("\n        - type: command");
                out.push_str(&format!("\n          command: {}", cmd.command));
                if cmd.timeout != 60 {
                    out.push_str(&format!("\n          timeout: {}", cmd.timeout));
                }
            }
        }
    };

    write_entries(&mut out, "PreToolUse", &hooks.pre_tool_use);
    write_entries(&mut out, "PostToolUse", &hooks.post_tool_use);
    write_entries(&mut out, "Stop", &hooks.stop);

    out
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

/// Load the project CLAUDE.md and resolve any `@path/to/file.md` imports within it.
///
/// - Reads `{git_root}/CLAUDE.md`; returns `None` and prints a warning if missing.
/// - Scans each line for `@(\S+\.md)` patterns; for each unique path (in order),
///   reads `{git_root}/{path}`. On failure, prints a warning and skips.
/// - Returns concatenation: CLAUDE.md content + `\n\n` + each imported file's content,
///   separated by `\n\n`.
pub fn load_project_config(git_root: &std::path::Path) -> Option<String> {
    let claude_path = git_root.join("CLAUDE.md");
    let claude_content = match std::fs::read_to_string(&claude_path) {
        Ok(c) => c,
        Err(_) => {
            eprintln!("warning: CLAUDE.md not found at {}", claude_path.display());
            return None;
        }
    };

    // Collect @-imports: find all `@<something>.md` patterns, deduped in order.
    let mut seen = std::collections::HashSet::new();
    let mut imports: Vec<String> = Vec::new();
    for line in claude_content.lines() {
        let mut rest = line;
        while let Some(at_pos) = rest.find('@') {
            let after_at = &rest[at_pos + 1..];
            // Extract the path: non-whitespace characters ending with `.md`
            let path_candidate: String =
                after_at.chars().take_while(|c| !c.is_whitespace()).collect();
            if path_candidate.ends_with(".md")
                    && !path_candidate.is_empty()
                    && seen.insert(path_candidate.clone())
                {
                    imports.push(path_candidate);
                }
            // Advance past the `@` we just processed
            rest = &rest[at_pos + 1..];
        }
    }

    // Load each imported file
    let mut parts: Vec<String> = vec![claude_content];
    for import_path in &imports {
        let full_path = git_root.join(import_path);
        match std::fs::read_to_string(&full_path) {
            Ok(content) => parts.push(content),
            Err(_) => {
                eprintln!(
                    "warning: could not read @-import '{}' ({})",
                    import_path,
                    full_path.display()
                );
            }
        }
    }

    Some(parts.join("\n\n"))
}

/// Load hooks from `{git_root}/.claude/settings.json`.
///
/// The settings.json format mirrors Claude Code's hook schema:
/// ```json
/// {
///   "hooks": {
///     "PreToolUse": [ { "matcher": "...", "hooks": [{"type":"command","command":"...","timeout":N}] } ],
///     "PostToolUse": [ ... ],
///     "Stop":        [ { "hooks": [...] } ]
///   }
/// }
/// ```
///
/// Returns `AgentHooks::default()` if the file is absent, unreadable, or has no `hooks` key.
/// Any parse error is logged as a warning to stderr and returns default.
pub fn load_project_hooks(git_root: &std::path::Path) -> AgentHooks {
    let path = git_root.join(".claude").join("settings.json");

    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return AgentHooks::default(),
    };

    let value: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("warning: failed to parse {}: {e}", path.display());
            return AgentHooks::default();
        }
    };

    let hooks_value = match value.get("hooks") {
        Some(h) => h,
        None => return AgentHooks::default(),
    };

    // Deserialise via the same RawHooks shape used for YAML frontmatter.
    let raw: RawHooks = match serde_json::from_value(hooks_value.clone()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("warning: failed to parse hooks in {}: {e}", path.display());
            return AgentHooks::default();
        }
    };

    let convert_entries = |entries: Vec<RawHookEntry>| -> Vec<HookEntry> {
        entries
            .into_iter()
            .map(|e| HookEntry {
                matcher: e.matcher,
                hooks: e
                    .hooks
                    .into_iter()
                    .map(|c| HookCommand { command: c.command, timeout: c.timeout })
                    .collect(),
            })
            .collect()
    };

    AgentHooks {
        pre_tool_use: convert_entries(raw.pre_tool_use),
        post_tool_use: convert_entries(raw.post_tool_use),
        stop: convert_entries(raw.stop),
    }
}

/// Merge `agent` hooks with `project` hooks.
///
/// Strategy for each event type (pre_tool_use, post_tool_use, stop):
/// 1. Start with all agent entries as-is.
/// 2. For each project entry, append it only if no agent entry has the same matcher value.
///    (Agent entries always win for matching matchers.)
pub fn merge_hooks(agent: AgentHooks, project: AgentHooks) -> AgentHooks {
    let merge_entries = |mut base: Vec<HookEntry>, additions: Vec<HookEntry>| -> Vec<HookEntry> {
        for proj_entry in additions {
            // Check whether any agent entry has the same matcher
            let duplicate = base.iter().any(|a| a.matcher == proj_entry.matcher);
            if !duplicate {
                base.push(proj_entry);
            }
        }
        base
    };

    AgentHooks {
        pre_tool_use: merge_entries(agent.pre_tool_use, project.pre_tool_use),
        post_tool_use: merge_entries(agent.post_tool_use, project.post_tool_use),
        stop: merge_entries(agent.stop, project.stop),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn make_def_no_hooks(name: &str) -> AgentDef {
        AgentDef {
            name: name.to_string(),
            description: "desc".to_string(),
            body: "body".to_string(),
            include_project_config: false,
            hooks: AgentHooks::default(),
        }
    }

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
            include_project_config: false,
            hooks: AgentHooks::default(),
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
            include_project_config: false,
            hooks: AgentHooks::default(),
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

        write_agent(dir, &make_def_no_hooks("zebra")).unwrap();
        write_agent(dir, &make_def_no_hooks("alpha")).unwrap();
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

    #[test]
    fn agents_dir_matches_git_root_join() {
        let expected = workspace::git_root().map(|r| r.join(".ns2").join("agents"));
        let actual = agents_dir();
        assert_eq!(actual, expected, "agents_dir() must equal git_root().join('.ns2').join('agents')");
    }

    #[test]
    fn agents_dir_returns_absolute_path() {
        let dir = match agents_dir() {
            Some(d) => d,
            None => return,
        };
        assert!(dir.is_absolute(), "agents_dir() must return an absolute path, got: {}", dir.display());
    }

    #[test]
    fn parse_agent_content_multiline_body() {
        let content = "---\nname: multi\ndescription: test\n---\n\nLine one.\nLine two.\n";
        let def = parse_agent_content(content).unwrap();
        assert_eq!(def.body, "Line one.\nLine two.");
    }

    #[test]
    fn format_agent_file_starts_with_frontmatter_delimiter() {
        let def = make_def_no_hooks("agent");
        let formatted = format_agent_file(&def);
        assert!(formatted.starts_with("---\n"), "formatted output must start with '---\\n'");
    }

    #[test]
    fn load_agent_returns_none_for_invalid_content() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("bad.md"), "not valid frontmatter").unwrap();
        assert!(load_agent(tmp.path(), "bad").is_none());
    }

    #[test]
    fn parse_agent_content_body_with_horizontal_rule_preserved() {
        let content = "---\nname: agent\ndescription: desc\n---\n\nSection one.\n\n---\n\nSection two.\n";
        let def = parse_agent_content(content).unwrap();
        assert_eq!(def.name, "agent");
        assert_eq!(def.body, "Section one.\n\n---\n\nSection two.");
    }

    #[test]
    fn list_agents_skips_invalid_frontmatter_md_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();

        write_agent(dir, &make_def_no_hooks("good")).unwrap();
        std::fs::write(dir.join("bad.md"), "not valid frontmatter at all").unwrap();

        let agents = list_agents(dir);
        assert_eq!(agents.len(), 1, "invalid .md file must be silently skipped");
        assert_eq!(agents[0].name, "good");
    }

    #[test]
    fn parse_agent_content_crlf_returns_none() {
        let content = "---\r\nname: agent\r\ndescription: desc\r\n---\r\n\r\nBody.\r\n";
        assert!(
            parse_agent_content(content).is_none(),
            "CRLF line endings are not supported and must return None"
        );
    }

    #[test]
    fn write_agent_creates_correct_filename() {
        let tmp = tempfile::TempDir::new().unwrap();
        write_agent(tmp.path(), &make_def_no_hooks("mybot")).unwrap();
        assert!(
            tmp.path().join("mybot.md").exists(),
            "write_agent must create {{name}}.md"
        );
    }

    // ── include_project_config field tests ───────────────────────────────────

    #[test]
    fn parse_agent_content_include_project_config_true_round_trips() {
        let content =
            "---\nname: agent\ndescription: desc\ninclude_project_config: true\n---\n\nBody.\n";
        let def = parse_agent_content(content).unwrap();
        assert_eq!(def.name, "agent");
        assert!(def.include_project_config, "include_project_config must be true");

        let formatted = format_agent_file(&def);
        assert!(
            formatted.contains("include_project_config: true"),
            "formatted output must contain 'include_project_config: true' when true"
        );
        let reparsed = parse_agent_content(&formatted).unwrap();
        assert!(reparsed.include_project_config, "re-parsed value must be true");
    }

    #[test]
    fn parse_agent_content_include_project_config_defaults_to_false() {
        let content = "---\nname: agent\ndescription: desc\n---\n\nBody.\n";
        let def = parse_agent_content(content).unwrap();
        assert!(
            !def.include_project_config,
            "include_project_config must default to false when absent"
        );
    }

    #[test]
    fn format_agent_file_omits_include_project_config_when_false() {
        let def = make_def_no_hooks("agent");
        let formatted = format_agent_file(&def);
        assert!(
            !formatted.contains("include_project_config"),
            "formatted output must NOT contain 'include_project_config' when false, got: {formatted}"
        );
    }

    // ── load_project_config tests ────────────────────────────────────────────

    #[test]
    fn load_project_config_returns_none_when_claude_md_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let result = load_project_config(tmp.path());
        assert!(result.is_none(), "must return None when CLAUDE.md is absent");
    }

    #[test]
    fn load_project_config_returns_claude_md_content_with_no_imports() {
        let tmp = tempfile::TempDir::new().unwrap();
        let claude_content = "# Project Config\n\nSome instructions here.\n";
        std::fs::write(tmp.path().join("CLAUDE.md"), claude_content).unwrap();

        let result = load_project_config(tmp.path()).unwrap();
        assert_eq!(result, claude_content, "result must equal CLAUDE.md content when no @-imports");
    }

    #[test]
    fn load_project_config_resolves_at_imports_mid_line() {
        let tmp = tempfile::TempDir::new().unwrap();

        std::fs::create_dir_all(tmp.path().join("docs")).unwrap();
        std::fs::write(tmp.path().join("docs/style.md"), "Style guide content.\n").unwrap();

        let claude_content = "See @docs/style.md here for style.\n";
        std::fs::write(tmp.path().join("CLAUDE.md"), claude_content).unwrap();

        let result = load_project_config(tmp.path()).unwrap();
        assert!(result.contains("See @docs/style.md here for style."));
        assert!(result.contains("Style guide content."));
        let claude_pos = result.find("See @docs/style.md").unwrap();
        let import_pos = result.find("Style guide content.").unwrap();
        assert!(claude_pos < import_pos);
    }

    #[test]
    fn load_project_config_deduplicates_at_imports() {
        let tmp = tempfile::TempDir::new().unwrap();

        std::fs::write(tmp.path().join("shared.md"), "Shared content.\n").unwrap();

        let claude_content = "First mention @shared.md\nSecond mention @shared.md\n";
        std::fs::write(tmp.path().join("CLAUDE.md"), claude_content).unwrap();

        let result = load_project_config(tmp.path()).unwrap();
        let count = result.matches("Shared content.").count();
        assert_eq!(count, 1, "imported file content must appear exactly once (deduped), got {count}");
    }

    #[test]
    fn load_project_config_warns_on_missing_import_returns_partial() {
        let tmp = tempfile::TempDir::new().unwrap();

        std::fs::write(tmp.path().join("real.md"), "Real content.\n").unwrap();
        let claude_content = "See @real.md and @absent-file.md\n";
        std::fs::write(tmp.path().join("CLAUDE.md"), claude_content).unwrap();

        let result = load_project_config(tmp.path());
        assert!(result.is_some());

        let content = result.unwrap();
        assert!(content.contains("Real content."));
        let real_count = content.matches("Real content.").count();
        assert_eq!(real_count, 1);
    }

    #[test]
    fn load_project_config_resolves_multiple_distinct_imports_in_order() {
        let tmp = tempfile::TempDir::new().unwrap();

        std::fs::write(tmp.path().join("first.md"), "First file.\n").unwrap();
        std::fs::write(tmp.path().join("second.md"), "Second file.\n").unwrap();

        let claude_content = "@first.md\n@second.md\n";
        std::fs::write(tmp.path().join("CLAUDE.md"), claude_content).unwrap();

        let result = load_project_config(tmp.path()).unwrap();
        let first_pos = result.find("First file.").unwrap();
        let second_pos = result.find("Second file.").unwrap();
        assert!(first_pos < second_pos);
    }

    // ── load_project_hooks tests (GH #33) ────────────────────────────────────

    /// Returns default (all-empty) when .claude/settings.json is absent.
    #[test]
    fn load_project_hooks_returns_default_when_file_absent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let hooks = load_project_hooks(tmp.path());
        assert!(hooks.pre_tool_use.is_empty(), "pre_tool_use must be empty");
        assert!(hooks.post_tool_use.is_empty(), "post_tool_use must be empty");
        assert!(hooks.stop.is_empty(), "stop must be empty");
    }

    /// Parses PreToolUse entries from .claude/settings.json correctly.
    #[test]
    fn load_project_hooks_parses_pre_tool_use() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".claude")).unwrap();
        std::fs::write(
            tmp.path().join(".claude/settings.json"),
            r#"{
                "hooks": {
                    "PreToolUse": [
                        {
                            "matcher": "Bash",
                            "hooks": [
                                {"type": "command", "command": "/pre.sh", "timeout": 300}
                            ]
                        }
                    ]
                }
            }"#,
        )
        .unwrap();

        let hooks = load_project_hooks(tmp.path());
        assert_eq!(hooks.pre_tool_use.len(), 1, "expected 1 PreToolUse entry");
        assert_eq!(hooks.pre_tool_use[0].matcher.as_deref(), Some("Bash"));
        assert_eq!(hooks.pre_tool_use[0].hooks.len(), 1);
        assert_eq!(hooks.pre_tool_use[0].hooks[0].command, "/pre.sh");
        assert_eq!(hooks.pre_tool_use[0].hooks[0].timeout, 300);
        assert!(hooks.post_tool_use.is_empty());
        assert!(hooks.stop.is_empty());
    }

    /// Parses PostToolUse entries correctly; timeout defaults to 60 when absent.
    #[test]
    fn load_project_hooks_parses_post_tool_use_with_default_timeout() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".claude")).unwrap();
        std::fs::write(
            tmp.path().join(".claude/settings.json"),
            r#"{
                "hooks": {
                    "PostToolUse": [
                        {
                            "matcher": ".*",
                            "hooks": [
                                {"type": "command", "command": "/post.sh"}
                            ]
                        }
                    ]
                }
            }"#,
        )
        .unwrap();

        let hooks = load_project_hooks(tmp.path());
        assert_eq!(hooks.post_tool_use.len(), 1, "expected 1 PostToolUse entry");
        assert_eq!(hooks.post_tool_use[0].matcher.as_deref(), Some(".*"));
        assert_eq!(hooks.post_tool_use[0].hooks[0].command, "/post.sh");
        assert_eq!(hooks.post_tool_use[0].hooks[0].timeout, 60, "default timeout must be 60");
    }

    /// Parses Stop entries (no matcher field) correctly.
    #[test]
    fn load_project_hooks_parses_stop_entries() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".claude")).unwrap();
        std::fs::write(
            tmp.path().join(".claude/settings.json"),
            r#"{
                "hooks": {
                    "Stop": [
                        {
                            "hooks": [
                                {"type": "command", "command": "/stop.sh"}
                            ]
                        }
                    ]
                }
            }"#,
        )
        .unwrap();

        let hooks = load_project_hooks(tmp.path());
        assert_eq!(hooks.stop.len(), 1, "expected 1 Stop entry");
        assert!(hooks.stop[0].matcher.is_none(), "Stop entry must have no matcher");
        assert_eq!(hooks.stop[0].hooks[0].command, "/stop.sh");
    }

    /// Returns default when JSON is syntactically invalid.
    #[test]
    fn load_project_hooks_returns_default_on_invalid_json() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".claude")).unwrap();
        std::fs::write(tmp.path().join(".claude/settings.json"), "not valid json { {").unwrap();

        let hooks = load_project_hooks(tmp.path());
        assert!(hooks.pre_tool_use.is_empty(), "must return default on parse error");
    }

    /// Returns default when JSON is valid but has no `hooks` key.
    #[test]
    fn load_project_hooks_returns_default_when_no_hooks_key() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".claude")).unwrap();
        std::fs::write(
            tmp.path().join(".claude/settings.json"),
            r#"{"someOtherKey": true}"#,
        )
        .unwrap();

        let hooks = load_project_hooks(tmp.path());
        assert!(hooks.pre_tool_use.is_empty(), "must return default when no hooks key");
        assert!(hooks.post_tool_use.is_empty());
        assert!(hooks.stop.is_empty());
    }

    // ── merge_hooks tests (GH #33) ────────────────────────────────────────────

    /// Agent entries appear in output unchanged.
    #[test]
    fn merge_hooks_agent_entries_preserved() {
        let agent = AgentHooks {
            pre_tool_use: vec![HookEntry {
                matcher: Some("bash".to_string()),
                hooks: vec![HookCommand { command: "/agent-pre.sh".to_string(), timeout: 10 }],
            }],
            ..AgentHooks::default()
        };
        let project = AgentHooks::default();

        let merged = merge_hooks(agent, project);
        assert_eq!(merged.pre_tool_use.len(), 1);
        assert_eq!(merged.pre_tool_use[0].matcher.as_deref(), Some("bash"));
        assert_eq!(merged.pre_tool_use[0].hooks[0].command, "/agent-pre.sh");
    }

    /// Project entries with new matchers are appended after agent entries.
    #[test]
    fn merge_hooks_project_entries_with_new_matchers_are_appended() {
        let agent = AgentHooks {
            pre_tool_use: vec![HookEntry {
                matcher: Some("bash".to_string()),
                hooks: vec![HookCommand { command: "/agent-pre.sh".to_string(), timeout: 10 }],
            }],
            ..AgentHooks::default()
        };
        let project = AgentHooks {
            pre_tool_use: vec![HookEntry {
                matcher: Some("read".to_string()),
                hooks: vec![HookCommand { command: "/proj-pre.sh".to_string(), timeout: 60 }],
            }],
            ..AgentHooks::default()
        };

        let merged = merge_hooks(agent, project);
        assert_eq!(merged.pre_tool_use.len(), 2, "expected 2 entries: agent + project");
        // Agent entry first
        assert_eq!(merged.pre_tool_use[0].matcher.as_deref(), Some("bash"));
        assert_eq!(merged.pre_tool_use[0].hooks[0].command, "/agent-pre.sh");
        // Project entry appended
        assert_eq!(merged.pre_tool_use[1].matcher.as_deref(), Some("read"));
        assert_eq!(merged.pre_tool_use[1].hooks[0].command, "/proj-pre.sh");
    }

    /// Project entries with the same matcher as an agent entry are dropped.
    #[test]
    fn merge_hooks_project_entries_with_duplicate_matchers_are_dropped() {
        let agent = AgentHooks {
            post_tool_use: vec![HookEntry {
                matcher: Some(".*".to_string()),
                hooks: vec![HookCommand { command: "/agent-post.sh".to_string(), timeout: 10 }],
            }],
            ..AgentHooks::default()
        };
        let project = AgentHooks {
            post_tool_use: vec![HookEntry {
                matcher: Some(".*".to_string()),
                hooks: vec![HookCommand { command: "/proj-post.sh".to_string(), timeout: 60 }],
            }],
            ..AgentHooks::default()
        };

        let merged = merge_hooks(agent, project);
        assert_eq!(merged.post_tool_use.len(), 1, "duplicate project entry must be dropped");
        assert_eq!(merged.post_tool_use[0].hooks[0].command, "/agent-post.sh");
    }

    /// Stop hooks (None matcher): project stop entries appended when agent has none.
    #[test]
    fn merge_hooks_stop_project_entries_appended_when_agent_empty() {
        let agent = AgentHooks::default();
        let project = AgentHooks {
            stop: vec![HookEntry {
                matcher: None,
                hooks: vec![HookCommand { command: "/proj-stop.sh".to_string(), timeout: 60 }],
            }],
            ..AgentHooks::default()
        };

        let merged = merge_hooks(agent, project);
        assert_eq!(merged.stop.len(), 1, "project stop entry must be appended when agent has none");
        assert_eq!(merged.stop[0].hooks[0].command, "/proj-stop.sh");
    }

    /// Stop hooks: project entries dropped when agent already has a None-matcher stop entry.
    #[test]
    fn merge_hooks_stop_project_entries_dropped_when_agent_has_none_matcher() {
        let agent = AgentHooks {
            stop: vec![HookEntry {
                matcher: None,
                hooks: vec![HookCommand { command: "/agent-stop.sh".to_string(), timeout: 10 }],
            }],
            ..AgentHooks::default()
        };
        let project = AgentHooks {
            stop: vec![HookEntry {
                matcher: None,
                hooks: vec![HookCommand { command: "/proj-stop.sh".to_string(), timeout: 60 }],
            }],
            ..AgentHooks::default()
        };

        let merged = merge_hooks(agent, project);
        assert_eq!(merged.stop.len(), 1, "duplicate None-matcher stop entries must be dropped");
        assert_eq!(merged.stop[0].hooks[0].command, "/agent-stop.sh");
    }

    /// merge_hooks with empty agent and empty project returns empty.
    #[test]
    fn merge_hooks_both_empty_returns_empty() {
        let merged = merge_hooks(AgentHooks::default(), AgentHooks::default());
        assert!(merged.pre_tool_use.is_empty());
        assert!(merged.post_tool_use.is_empty());
        assert!(merged.stop.is_empty());
    }

    // ── Hook parsing tests (GH #33) ──────────────────────────────────────────

    /// Missing `hooks:` field in frontmatter → `AgentHooks::default()` (all empty vecs).
    #[test]
    fn parse_agent_content_missing_hooks_defaults_to_empty() {
        let content = "---\nname: agent\ndescription: desc\n---\n\nBody.\n";
        let def = parse_agent_content(content).unwrap();
        assert!(def.hooks.pre_tool_use.is_empty(), "pre_tool_use must be empty");
        assert!(def.hooks.post_tool_use.is_empty(), "post_tool_use must be empty");
        assert!(def.hooks.stop.is_empty(), "stop must be empty");
    }

    /// PreToolUse hook with matcher parses correctly.
    #[test]
    fn parse_agent_content_pre_tool_use_with_matcher() {
        let content = "\
---
name: agent
description: desc
hooks:
  PreToolUse:
    - matcher: \"bash\"
      hooks:
        - type: command
          command: /path/to/script.sh
          timeout: 10
---

Body.
";
        let def = parse_agent_content(content).unwrap();
        assert_eq!(def.hooks.pre_tool_use.len(), 1);
        let entry = &def.hooks.pre_tool_use[0];
        assert_eq!(entry.matcher.as_deref(), Some("bash"));
        assert_eq!(entry.hooks.len(), 1);
        assert_eq!(entry.hooks[0].command, "/path/to/script.sh");
        assert_eq!(entry.hooks[0].timeout, 10);
    }

    /// PostToolUse hook with matcher parses correctly.
    #[test]
    fn parse_agent_content_post_tool_use_with_matcher() {
        let content = "\
---
name: agent
description: desc
hooks:
  PostToolUse:
    - matcher: \".*\"
      hooks:
        - type: command
          command: /path/to/script.sh
---

Body.
";
        let def = parse_agent_content(content).unwrap();
        assert_eq!(def.hooks.post_tool_use.len(), 1);
        let entry = &def.hooks.post_tool_use[0];
        assert_eq!(entry.matcher.as_deref(), Some(".*"));
        assert_eq!(entry.hooks.len(), 1);
        assert_eq!(entry.hooks[0].command, "/path/to/script.sh");
        assert_eq!(entry.hooks[0].timeout, 60, "default timeout must be 60");
    }

    /// Stop hook without matcher parses correctly.
    #[test]
    fn parse_agent_content_stop_hook_without_matcher() {
        let content = "\
---
name: agent
description: desc
hooks:
  Stop:
    - hooks:
        - type: command
          command: /path/to/script.sh
          timeout: 30
---

Body.
";
        let def = parse_agent_content(content).unwrap();
        assert_eq!(def.hooks.stop.len(), 1);
        let entry = &def.hooks.stop[0];
        assert!(entry.matcher.is_none(), "Stop hook matcher must be None");
        assert_eq!(entry.hooks.len(), 1);
        assert_eq!(entry.hooks[0].command, "/path/to/script.sh");
        assert_eq!(entry.hooks[0].timeout, 30);
    }

    /// Hooks round-trip through parse → format → parse.
    #[test]
    fn hooks_frontmatter_round_trips() {
        let content = "\
---
name: agent
description: desc
hooks:
  PreToolUse:
    - matcher: \"bash\"
      hooks:
        - type: command
          command: /path/to/script.sh
          timeout: 10
  PostToolUse:
    - matcher: \".*\"
      hooks:
        - type: command
          command: /other/script.sh
  Stop:
    - hooks:
        - type: command
          command: /stop/script.sh
          timeout: 30
---

Body.
";
        let def = parse_agent_content(content).unwrap();
        let formatted = format_agent_file(&def);
        let reparsed = parse_agent_content(&formatted).unwrap();

        // PreToolUse
        assert_eq!(reparsed.hooks.pre_tool_use.len(), 1);
        assert_eq!(reparsed.hooks.pre_tool_use[0].matcher.as_deref(), Some("bash"));
        assert_eq!(reparsed.hooks.pre_tool_use[0].hooks[0].command, "/path/to/script.sh");
        assert_eq!(reparsed.hooks.pre_tool_use[0].hooks[0].timeout, 10);

        // PostToolUse
        assert_eq!(reparsed.hooks.post_tool_use.len(), 1);
        assert_eq!(reparsed.hooks.post_tool_use[0].matcher.as_deref(), Some(".*"));
        assert_eq!(reparsed.hooks.post_tool_use[0].hooks[0].command, "/other/script.sh");
        assert_eq!(reparsed.hooks.post_tool_use[0].hooks[0].timeout, 60);

        // Stop
        assert_eq!(reparsed.hooks.stop.len(), 1);
        assert!(reparsed.hooks.stop[0].matcher.is_none());
        assert_eq!(reparsed.hooks.stop[0].hooks[0].command, "/stop/script.sh");
        assert_eq!(reparsed.hooks.stop[0].hooks[0].timeout, 30);
    }

    /// format_agent_file omits the `hooks:` section entirely when hooks are empty.
    #[test]
    fn format_agent_file_omits_hooks_when_empty() {
        let def = make_def_no_hooks("agent");
        let formatted = format_agent_file(&def);
        assert!(
            !formatted.contains("hooks:"),
            "formatted output must NOT contain 'hooks:' when hooks are empty, got:\n{formatted}"
        );
    }

    /// A full agent file with all three hook types serializes and parses back correctly.
    #[test]
    fn write_then_load_agent_with_hooks_round_trips() {
        let tmp = tempfile::TempDir::new().unwrap();
        let def = AgentDef {
            name: "hooked".to_string(),
            description: "agent with hooks".to_string(),
            body: "body text".to_string(),
            include_project_config: false,
            hooks: AgentHooks {
                pre_tool_use: vec![HookEntry {
                    matcher: Some("bash".to_string()),
                    hooks: vec![HookCommand { command: "/pre.sh".to_string(), timeout: 5 }],
                }],
                post_tool_use: vec![HookEntry {
                    matcher: Some(".*".to_string()),
                    hooks: vec![HookCommand { command: "/post.sh".to_string(), timeout: 60 }],
                }],
                stop: vec![HookEntry {
                    matcher: None,
                    hooks: vec![HookCommand { command: "/stop.sh".to_string(), timeout: 20 }],
                }],
            },
        };

        write_agent(tmp.path(), &def).unwrap();
        let loaded = load_agent(tmp.path(), "hooked").unwrap();

        assert_eq!(loaded.hooks.pre_tool_use.len(), 1);
        assert_eq!(loaded.hooks.pre_tool_use[0].matcher.as_deref(), Some("bash"));
        assert_eq!(loaded.hooks.pre_tool_use[0].hooks[0].command, "/pre.sh");
        assert_eq!(loaded.hooks.pre_tool_use[0].hooks[0].timeout, 5);

        assert_eq!(loaded.hooks.post_tool_use.len(), 1);
        assert_eq!(loaded.hooks.post_tool_use[0].matcher.as_deref(), Some(".*"));
        assert_eq!(loaded.hooks.post_tool_use[0].hooks[0].command, "/post.sh");
        assert_eq!(loaded.hooks.post_tool_use[0].hooks[0].timeout, 60);

        assert_eq!(loaded.hooks.stop.len(), 1);
        assert!(loaded.hooks.stop[0].matcher.is_none());
        assert_eq!(loaded.hooks.stop[0].hooks[0].command, "/stop.sh");
        assert_eq!(loaded.hooks.stop[0].hooks[0].timeout, 20);
    }
}
