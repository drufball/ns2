use assert_cmd::cargo::cargo_bin;
use std::collections::HashSet;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Condvar, LazyLock, Mutex};
use tempfile::TempDir;

pub struct TestHarness {
    pub home_dir: TempDir,
    pub repo_dir: TempDir,
    pub port: u16,
    server: Option<Child>,
}

#[allow(dead_code)]
impl TestHarness {
    /// Create a new harness with a git-initialised temp repo. No server started.
    pub fn new() -> Self {
        let home_dir = TempDir::new().unwrap();
        let repo_dir = TempDir::new().unwrap();
        git_init(repo_dir.path(), home_dir.path());
        Self {
            home_dir,
            repo_dir,
            port: free_port(),
            server: None,
        }
    }

    /// Start the ns2 server on `self.port` and block until it is ready.
    ///
    /// At most `SERVER_CONCURRENCY` servers are started simultaneously to avoid
    /// port-binding races when many tests run in parallel.
    pub fn start_server(&mut self) {
        // Acquire a concurrency slot before spawning; release once server is up.
        let permit = SERVER_SEMAPHORE.acquire();

        let proc = Command::new(cargo_bin("ns2"))
            .args(["server", "start", "--port", &self.port.to_string()])
            .env("HOME", self.home_dir.path())
            .env_remove("ANTHROPIC_API_KEY")
            .current_dir(self.repo_dir.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn ns2 server");

        let addr = format!("127.0.0.1:{}", self.port);
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if std::net::TcpStream::connect(&addr).is_ok() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "server did not accept connections within 10s on {addr}"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // Release the slot once the server is confirmed up.
        drop(permit);
        self.server = Some(proc);
    }

    /// Returns a pre-configured `assert_cmd::Command` for the ns2 binary.
    /// The command targets `self.port` and has HOME set to the test home dir.
    /// Chain `.arg()` / `.args()` to specify the subcommand.
    pub fn ns2(&self) -> assert_cmd::Command {
        let mut cmd = assert_cmd::Command::new(cargo_bin("ns2"));
        cmd.arg("--server")
            .arg(format!("http://127.0.0.1:{}", self.port))
            .env("HOME", self.home_dir.path())
            .env_remove("ANTHROPIC_API_KEY")
            .current_dir(self.repo_dir.path());
        cmd
    }

    /// Run ns2 and return stdout as a trimmed String. Panics on non-zero exit.
    pub fn ns2_stdout(&self, args: &[&str]) -> String {
        let out = Command::new(cargo_bin("ns2"))
            .arg("--server")
            .arg(format!("http://127.0.0.1:{}", self.port))
            .env("HOME", self.home_dir.path())
            .env_remove("ANTHROPIC_API_KEY")
            .current_dir(self.repo_dir.path())
            .args(args)
            .output()
            .expect("failed to run ns2");
        assert!(
            out.status.success(),
            "ns2 {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    /// Create seed test files and commit them.
    pub fn seed_files(&self) {
        std::fs::write(
            self.repo_dir.path().join("read-test.txt"),
            "The secret value is: ns2-read-tool-test-42\n",
        )
        .unwrap();
        std::fs::write(
            self.repo_dir.path().join("multi-turn-test.txt"),
            "The magic number is: 7742\n",
        )
        .unwrap();
        self.git(&["add", "."]);
        self.git(&["commit", "-m", "seed test files"]);
    }

    /// Create a codebase-like directory layout and commit it.
    pub fn setup_codebase_layout(&self) {
        let root = self.repo_dir.path();
        let crates = root.join("crates");
        std::fs::create_dir_all(crates.join("cli/src")).unwrap();
        std::fs::write(crates.join("cli/src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::create_dir_all(crates.join("agents/src")).unwrap();
        std::fs::write(crates.join("agents/src/lib.rs"), "pub fn hello() {}\n").unwrap();
        std::fs::create_dir_all(crates.join("arch-tests")).unwrap();
        std::fs::write(
            crates.join("arch-tests/architecture.spec.md"),
            "# Architecture Specification\n\nPlain doc without targets frontmatter.\n",
        )
        .unwrap();
        self.git(&["add", "."]);
        self.git(&["commit", "-m", "add codebase layout"]);
    }

    /// Set up a local bare clone as the `origin` remote (required for worktree tests).
    pub fn setup_origin(&self) {
        let bare = self.home_dir.path().join("origin-bare");
        Command::new("git")
            .args([
                "clone",
                "--bare",
                self.repo_dir.path().to_str().unwrap(),
                bare.to_str().unwrap(),
            ])
            .env("HOME", self.home_dir.path())
            .output()
            .unwrap();
        self.git(&["remote", "add", "origin", bare.to_str().unwrap()]);
        self.git(&["fetch", "origin"]);
        self.git(&["remote", "set-head", "origin", "--auto"]);
    }

    /// Blocking HTTP GET; returns the response body as a String. Panics on error.
    pub fn http_get(&self, path: &str) -> String {
        let url = format!("http://127.0.0.1:{}{}", self.port, path);
        ureq::get(&url).call().unwrap().into_string().unwrap()
    }

    /// Blocking HTTP PATCH with a JSON body. Panics on error.
    pub fn http_patch(&self, path: &str, body: &str) {
        let url = format!("http://127.0.0.1:{}{}", self.port, path);
        ureq::request("PATCH", &url)
            .set("Content-Type", "application/json")
            .send_string(body)
            .unwrap();
    }

    /// Blocking HTTP POST with a JSON body; returns the response body. Panics on error.
    pub fn http_post(&self, path: &str, body: &str) -> String {
        let url = format!("http://127.0.0.1:{}{}", self.port, path);
        ureq::post(&url)
            .set("Content-Type", "application/json")
            .send_string(body)
            .unwrap()
            .into_string()
            .unwrap()
    }

    /// Run a git command in the repo dir and return the Output.
    pub fn git(&self, args: &[&str]) -> std::process::Output {
        Command::new("git")
            .args(args)
            .env("HOME", self.home_dir.path())
            .current_dir(self.repo_dir.path())
            .output()
            .unwrap()
    }

    /// Kill the running server and wait for it to exit. After this call,
    /// `start_server` may be called again to bring up a fresh server on the
    /// same port with the same data directory.
    pub fn stop_server(&mut self) {
        if let Some(mut proc) = self.server.take() {
            proc.kill().ok();
            proc.wait().ok();
        }
    }

    /// Returns the worktree base path: `$HOME/.ns2/<repo-name>/worktrees/`
    pub fn worktree_base(&self) -> PathBuf {
        let repo_name = self
            .repo_dir
            .path()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        self.home_dir
            .path()
            .join(".ns2")
            .join(repo_name)
            .join("worktrees")
    }
}

impl Drop for TestHarness {
    fn drop(&mut self) {
        if let Some(mut proc) = self.server.take() {
            proc.kill().ok();
            proc.wait().ok();
        }
    }
}

// ── Startup concurrency limiter ───────────────────────────────────────────────

/// Maximum number of ns2 server processes that may be starting up at the same
/// time.  Keeping this below the number of CPU cores prevents port-binding
/// races: when too many servers initialise concurrently the window between
/// `free_port()` releasing the OS listener and the server's own `bind()` grows
/// large enough that macOS reassigns the ephemeral port to another process.
const SERVER_CONCURRENCY: usize = 2;

struct Semaphore {
    inner: Mutex<usize>,
    condvar: Condvar,
}

struct SemaphorePermit<'a>(&'a Semaphore);

impl Semaphore {
    const fn new() -> Self {
        Self {
            inner: Mutex::new(SERVER_CONCURRENCY),
            condvar: Condvar::new(),
        }
    }

    fn acquire(&self) -> SemaphorePermit<'_> {
        let mut count = self.inner.lock().unwrap();
        while *count == 0 {
            count = self.condvar.wait(count).unwrap();
        }
        *count -= 1;
        drop(count);
        SemaphorePermit(self)
    }
}

impl Drop for SemaphorePermit<'_> {
    fn drop(&mut self) {
        let mut count = self.0.inner.lock().unwrap();
        *count += 1;
        drop(count);
        self.0.condvar.notify_one();
    }
}

static SERVER_SEMAPHORE: Semaphore = Semaphore::new();

// Global set of ports already handed out within this test run.
// Prevents two concurrent tests from racing to the same ephemeral port.
static ALLOCATED_PORTS: LazyLock<Mutex<HashSet<u16>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

fn free_port() -> u16 {
    let mut allocated = ALLOCATED_PORTS.lock().unwrap();
    loop {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        if allocated.insert(port) {
            return port;
        }
    }
}

fn git_init(repo: &Path, home: &Path) {
    Command::new("git")
        .args(["init", repo.to_str().unwrap()])
        .env("HOME", home)
        .output()
        .unwrap();
    Command::new("git")
        .args([
            "-C",
            repo.to_str().unwrap(),
            "config",
            "user.email",
            "test@example.com",
        ])
        .env("HOME", home)
        .output()
        .unwrap();
    Command::new("git")
        .args([
            "-C",
            repo.to_str().unwrap(),
            "config",
            "user.name",
            "ns2 tester",
        ])
        .env("HOME", home)
        .output()
        .unwrap();
    std::fs::write(repo.join("README.md"), "# ns2-test-repo\n").unwrap();
    Command::new("git")
        .args(["-C", repo.to_str().unwrap(), "add", "README.md"])
        .env("HOME", home)
        .output()
        .unwrap();
    Command::new("git")
        .args([
            "-C",
            repo.to_str().unwrap(),
            "commit",
            "-m",
            "initial commit",
        ])
        .env("HOME", home)
        .output()
        .unwrap();
}
