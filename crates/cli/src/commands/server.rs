use std::path::PathBuf;

pub fn data_dir_and_pid(port: u16) -> (PathBuf, PathBuf) {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let repo_name = workspace::git_root_sync()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_else(|| "default".to_string());

    let data_dir = PathBuf::from(&home).join(".ns2").join(&repo_name);
    let pid_file = data_dir.join(format!("server-{port}.pid"));
    (data_dir, pid_file)
}

pub async fn run_start(port: u16) {
    let (data_dir, pid_file) = data_dir_and_pid(port);
    let config = server::ServerConfig {
        port,
        data_dir,
        pid_file,
        model: std::env::var("ANTHROPIC_MODEL")
            .unwrap_or_else(|_| "claude-sonnet-4-6".to_string()),
    };
    if let Err(e) = server::run(config).await {
        eprintln!("Server error: {e}");
        std::process::exit(1);
    }
}

pub fn run_stop(port: u16) {
    let (_, pid_file) = data_dir_and_pid(port);
    if let Ok(pid_str) = std::fs::read_to_string(&pid_file) {
        let pid = pid_str.trim().to_string();
        // Use sh to invoke kill so the shell builtin is available
        // even on minimal systems without a standalone kill binary.
        let result = std::process::Command::new("sh")
            .args(["-c", &format!("kill -TERM {pid}")])
            .output();
        match result {
            Ok(o) if o.status.success() => {
                eprintln!("Server stopped (pid {pid})");
            }
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                if stderr.contains("No such process") {
                    // Stale PID file — process already gone
                    let _ = std::fs::remove_file(&pid_file);
                    eprintln!("Warning: server process {pid} was not running (stale PID file removed)");
                } else {
                    eprintln!("Failed to stop server: {stderr}");
                    std::process::exit(1);
                }
            }
            Err(e) => {
                eprintln!("Failed to send signal: {e}");
                std::process::exit(1);
            }
        }
    } else {
        eprintln!("No PID file found at {}", pid_file.display());
        std::process::exit(1);
    }
}
