pub(crate) fn run_list() {
    let dir = agents::agents_dir().unwrap_or_else(|| {
        eprintln!("Error: not inside a git repository");
        std::process::exit(1);
    });
    if !dir.exists() {
        println!("No agents found (directory does not exist: {})", dir.display());
        return;
    }
    let agent_list = agents::list_agents(&dir);
    if agent_list.is_empty() {
        println!("No agents found.");
    } else {
        println!("{:<20} description", "name");
        for a in &agent_list {
            println!("{:<20} {}", a.name, a.description);
        }
    }
}

pub(crate) fn run_new(name: Option<String>, description: Option<String>, body: Option<String>) {
    let name = name.unwrap_or_else(|| {
        eprintln!("Error: --name is required");
        std::process::exit(1);
    });
    let dir = agents::agents_dir().unwrap_or_else(|| {
        eprintln!("Error: not inside a git repository");
        std::process::exit(1);
    });
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("Error creating agents directory: {e}");
        std::process::exit(1);
    }
    let path = dir.join(format!("{name}.md"));
    if path.exists() {
        eprintln!("Error: agent '{name}' already exists at {}", path.display());
        std::process::exit(1);
    }
    let open_editor = body.is_none();
    let def = agents::AgentDef {
        name: name.clone(),
        description: description.unwrap_or_default(),
        body: body.unwrap_or_default(),
        include_project_config: false,
        hooks: agents::AgentHooks::default(),
    };
    if let Err(e) = agents::write_agent(&dir, &def) {
        eprintln!("Error writing agent file: {e}");
        std::process::exit(1);
    }
    if open_editor {
        let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
        std::process::Command::new(&editor).arg(&path).status().ok();
    }
    eprintln!("Created agent '{name}' at {}", path.display());
}

pub(crate) fn run_edit(name: Option<String>, description: Option<String>, body: Option<String>) {
    let name = name.unwrap_or_else(|| {
        eprintln!("Error: --name is required");
        std::process::exit(1);
    });
    if description.is_none() && body.is_none() {
        eprintln!("Error: at least one of --description or --body must be provided");
        std::process::exit(1);
    }
    let dir = agents::agents_dir().unwrap_or_else(|| {
        eprintln!("Error: not inside a git repository");
        std::process::exit(1);
    });
    let mut def = agents::load_agent(&dir, &name).unwrap_or_else(|| {
        eprintln!(
            "Error: agent '{name}' not found at {}",
            dir.join(format!("{name}.md")).display()
        );
        std::process::exit(1);
    });
    if let Some(d) = description {
        def.description = d;
    }
    if let Some(b) = body {
        def.body = b;
    }
    if let Err(e) = agents::write_agent(&dir, &def) {
        eprintln!("Error writing agent file: {e}");
        std::process::exit(1);
    }
    eprintln!("Updated agent '{name}'.");
}
