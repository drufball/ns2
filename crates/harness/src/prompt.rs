use std::path::Path;

/// Build the preamble block that is prepended to every agent system prompt when
/// the git root is known. Returns `None` when the root cannot be determined so
/// callers can omit the preamble without failing.
pub(crate) fn build_preamble(root: &Path) -> String {
    let repo_name = root.file_name().unwrap_or_default().to_string_lossy();
    format!(
        "You are running in the ns2 agent harness.\nWorking directory / git root: {}\nRepository: {}\n",
        root.display(),
        repo_name,
    )
}

/// Assemble the final system prompt from an optional git root, an optional
/// agent definition directory, the session agent name, and the effective root
/// used for project config loading.
///
/// Returns `None` when there is nothing useful to send (no preamble, no agent body).
pub(crate) fn build_system_prompt(
    effective_root: Option<&Path>,
    agents_dir: Option<&Path>,
    agent_name: Option<&str>,
) -> Option<String> {
    // Build preamble if we know where we are.
    let preamble: Option<String> = effective_root.map(build_preamble);

    // Load agent body (+ optional project config).
    let agent_body_and_project: Option<String> = agent_name.and_then(|name| {
        let dir = agents_dir?;
        agents::load_agent(dir, name)
    }).and_then(|def| {
        let agent_body = def.body;
        if def.include_project_config {
            let project = effective_root
                .and_then(agents::load_project_config)
                .unwrap_or_default();
            if project.is_empty() {
                if agent_body.is_empty() { None } else { Some(agent_body) }
            } else {
                Some(format!("{agent_body}\n\n{project}"))
            }
        } else {
            if agent_body.is_empty() { None } else { Some(agent_body) }
        }
    });

    match (preamble, agent_body_and_project) {
        (Some(pre), Some(body)) => Some(format!("{pre}{body}")),
        (_,         None)       => None,   // no agent body → no system prompt, even with a preamble
        (None,      body)       => body,
    }
}
