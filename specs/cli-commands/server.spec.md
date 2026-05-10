---
targets:
  - crates/cli/src/commands/server.rs
verified: 2026-05-10T19:51:52Z
---

# ns2 server

The server is a lightweight localhost process that hosts session state and the agent event loop. Every other ns2 command talks to it over HTTP, so it must be running before anything else works.

## Starting and stopping

`ns2 server start` launches the process in the background and writes a PID file so stop knows what to kill. The default port is 9876. If that port is already in use — for example when running two separate ns2 repos on the same machine — pass `--port` to choose a different one.

`ns2 server stop` reads the PID file and sends a termination signal. The PID file lives at `~/.ns2/<repo-name>/server-<port>.pid`, so stopping on the default port looks for `server-9876.pid`.

## Typical usage

```bash
ns2 server start          # once per work session
# ... do work ...
ns2 server stop           # when done
```

You rarely need to think about the server after starting it. If a command fails with a connection error, the server probably isn't running.

## Configuration (ns2.toml)

On startup, `ns2 server start` reads `ns2.toml` from the git root (falling back to the current directory). The `[issues]` section configures the pluggable issue storage backend:

```toml
[issues]
backend = "sqlite"   # default; also "shell" or "github"

[issues.shell]
command = ".ns2/backends/my-backend.sh"

[issues.github]
owner = "myorg"
repo  = "myrepo"
```

If `ns2.toml` is absent or has no `[issues]` section, the SQLite backend is used. The GitHub backend additionally requires `GITHUB_TOKEN` in the environment.