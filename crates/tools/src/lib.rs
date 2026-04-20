use async_trait::async_trait;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("command timed out after {timeout_ms}ms")]
    Timeout { timeout_ms: u64 },
    #[error("command exited with code {code}: {output}")]
    ExitError { code: i32, output: String },
}

pub type Result<T> = std::result::Result<T, Error>;

#[async_trait]
pub trait Tool: Send + Sync {
    fn definition(&self) -> types::ToolDefinition;
    async fn execute(&self, input: serde_json::Value) -> Result<String>;
}

pub struct ReadTool;

#[async_trait]
impl Tool for ReadTool {
    fn definition(&self) -> types::ToolDefinition {
        types::ToolDefinition {
            name: "read".into(),
            description: "Read the contents of a file".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to file"
                    }
                },
                "required": ["path"]
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value) -> Result<String> {
        let path = input
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("missing required field: path".into()))?;

        let content = tokio::fs::read_to_string(path).await?;
        Ok(content)
    }
}

pub struct BashTool;

const DEFAULT_TIMEOUT_MS: u64 = 30_000;

#[async_trait]
impl Tool for BashTool {
    fn definition(&self) -> types::ToolDefinition {
        types::ToolDefinition {
            name: "bash".into(),
            description: "Execute a shell command and return its output".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The shell command to execute"
                    },
                    "timeout_ms": {
                        "type": "number",
                        "description": "Timeout in milliseconds (default: 30000)"
                    }
                },
                "required": ["command"]
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value) -> Result<String> {
        let command = input
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("missing required field: command".into()))?;

        let timeout_ms = input
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_MS);
        let timeout = Duration::from_millis(timeout_ms);

        let child = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(Error::Io)?;

        let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => return Err(Error::Io(e)),
            Err(_) => return Err(Error::Timeout { timeout_ms }),
        };

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

        let formatted = if stderr.is_empty() {
            stdout.clone()
        } else {
            format!("stdout:\n{stdout}stderr:\n{stderr}")
        };

        if output.status.success() {
            Ok(formatted)
        } else {
            let code = output.status.code().unwrap_or(-1);
            Err(Error::ExitError { code, output: formatted })
        }
    }
}

pub struct WriteTool;

#[async_trait]
impl Tool for WriteTool {
    fn definition(&self) -> types::ToolDefinition {
        types::ToolDefinition {
            name: "write".into(),
            description: "Write content to a file, creating parent directories if needed".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to write"
                    },
                    "content": {
                        "type": "string",
                        "description": "Content to write to the file"
                    }
                },
                "required": ["path", "content"]
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value) -> Result<String> {
        let path = input
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("missing required field: path".into()))?;

        let content = input
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("missing required field: content".into()))?;

        if let Some(parent) = std::path::Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent).await?;
            }
        }

        let bytes = content.as_bytes();
        tokio::fs::write(path, bytes).await?;

        Ok(format!("Wrote {} bytes to {}", bytes.len(), path))
    }
}

pub struct EditTool;

#[async_trait]
impl Tool for EditTool {
    fn definition(&self) -> types::ToolDefinition {
        types::ToolDefinition {
            name: "edit".into(),
            description: "Replace an exact string in a file".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to edit"
                    },
                    "old_string": {
                        "type": "string",
                        "description": "The exact string to replace"
                    },
                    "new_string": {
                        "type": "string",
                        "description": "The replacement string"
                    },
                    "replace_all": {
                        "type": "boolean",
                        "description": "Replace all occurrences (default: false)"
                    }
                },
                "required": ["path", "old_string", "new_string"]
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value) -> Result<String> {
        let path = input
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("missing required field: path".into()))?;

        let old_string = input
            .get("old_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("missing required field: old_string".into()))?;

        let new_string = input
            .get("new_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("missing required field: new_string".into()))?;

        let replace_all = input
            .get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let contents = tokio::fs::read_to_string(path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Error::InvalidInput(format!("file not found: {path}"))
            } else {
                Error::Io(e)
            }
        })?;

        if !contents.contains(old_string) {
            return Err(Error::InvalidInput(format!(
                "old_string not found in {path}"
            )));
        }

        let (new_contents, count) = if replace_all {
            let count = contents.matches(old_string).count();
            (contents.replace(old_string, new_string), count)
        } else {
            (contents.replacen(old_string, new_string, 1), 1)
        };

        tokio::fs::write(path, new_contents.as_bytes()).await?;

        Ok(format!("Replaced {count} occurrence(s) in {path}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[tokio::test]
    async fn read_tool_happy_path() {
        let mut tmp = tempfile::NamedTempFile::new().expect("create temp file");
        write!(tmp, "hello from file").expect("write temp file");
        let path = tmp.path().to_str().unwrap().to_owned();

        let tool = ReadTool;
        let result = tool.execute(serde_json::json!({"path": path})).await;
        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        assert_eq!(result.unwrap(), "hello from file");
    }

    #[tokio::test]
    async fn read_tool_file_not_found() {
        let tool = ReadTool;
        let result = tool
            .execute(serde_json::json!({"path": "/nonexistent/path/that/does/not/exist.txt"}))
            .await;
        assert!(result.is_err(), "expected Err for missing file");
        assert!(matches!(result.unwrap_err(), Error::Io(_)));
    }

    #[tokio::test]
    async fn read_tool_missing_path_field() {
        let tool = ReadTool;
        let result = tool.execute(serde_json::json!({})).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), Error::InvalidInput(_)));
    }

    #[test]
    fn read_tool_definition_has_correct_name() {
        let tool = ReadTool;
        let def = tool.definition();
        assert_eq!(def.name, "read");
    }

    #[test]
    fn read_tool_definition_schema_has_path_property() {
        let tool = ReadTool;
        let def = tool.definition();
        assert_eq!(def.input_schema["type"], "object");
        assert!(def.input_schema["properties"]["path"].is_object());
        let required = def.input_schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("path")));
    }

    // --- BashTool tests ---

    #[tokio::test]
    async fn bash_tool_happy_path() {
        let tool = BashTool;
        let result = tool
            .execute(serde_json::json!({"command": "echo hello"}))
            .await;
        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        // stderr is empty, so output should be plain stdout with no labels
        assert_eq!(result.unwrap(), "hello\n");
    }

    #[tokio::test]
    async fn bash_tool_stderr_labeled_in_output() {
        let tool = BashTool;
        // Write to both stdout and stderr
        let result = tool
            .execute(serde_json::json!({"command": "echo out; echo err >&2"}))
            .await;
        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        let output = result.unwrap();
        assert!(output.contains("stdout:\n"), "expected 'stdout:' label: {output}");
        assert!(output.contains("stderr:\n"), "expected 'stderr:' label: {output}");
        assert!(output.contains("out\n"), "expected stdout content: {output}");
        assert!(output.contains("err\n"), "expected stderr content: {output}");
    }

    #[tokio::test]
    async fn bash_tool_nonzero_exit_returns_exit_error() {
        let tool = BashTool;
        let result = tool
            .execute(serde_json::json!({"command": "exit 1"}))
            .await;
        assert!(result.is_err(), "expected Err for non-zero exit");
        assert!(
            matches!(result.unwrap_err(), Error::ExitError { code: 1, .. }),
            "expected ExitError with code 1"
        );
    }

    #[tokio::test]
    async fn bash_tool_timeout_returns_timeout_error() {
        let tool = BashTool;
        let result = tool
            .execute(serde_json::json!({"command": "sleep 60", "timeout_ms": 100}))
            .await;
        assert!(result.is_err(), "expected Err for timeout");
        assert!(
            matches!(result.unwrap_err(), Error::Timeout { timeout_ms: 100 }),
            "expected Timeout error"
        );
    }

    #[tokio::test]
    async fn bash_tool_missing_command_field() {
        let tool = BashTool;
        let result = tool.execute(serde_json::json!({})).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), Error::InvalidInput(_)));
    }

    #[test]
    fn bash_tool_definition_has_correct_name() {
        let tool = BashTool;
        let def = tool.definition();
        assert_eq!(def.name, "bash");
    }

    // --- WriteTool tests ---

    #[tokio::test]
    async fn write_tool_happy_path() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("out.txt");
        let path_str = path.to_str().unwrap().to_owned();

        let tool = WriteTool;
        let result = tool
            .execute(serde_json::json!({"path": path_str, "content": "hello write"}))
            .await;
        assert!(result.is_ok(), "expected Ok, got: {:?}", result);

        let written = tokio::fs::read_to_string(&path_str).await.unwrap();
        assert_eq!(written, "hello write");
    }

    #[tokio::test]
    async fn write_tool_creates_parent_dirs() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("subdir").join("nested").join("out.txt");
        let path_str = path.to_str().unwrap().to_owned();

        let tool = WriteTool;
        let result = tool
            .execute(serde_json::json!({"path": path_str, "content": "nested content"}))
            .await;
        assert!(result.is_ok(), "expected Ok, got: {:?}", result);

        let written = tokio::fs::read_to_string(&path_str).await.unwrap();
        assert_eq!(written, "nested content");
    }

    #[tokio::test]
    async fn write_tool_missing_path_field() {
        let tool = WriteTool;
        let result = tool
            .execute(serde_json::json!({"content": "some content"}))
            .await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), Error::InvalidInput(_)));
    }

    #[tokio::test]
    async fn write_tool_missing_content_field() {
        let tool = WriteTool;
        let result = tool
            .execute(serde_json::json!({"path": "/tmp/test.txt"}))
            .await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), Error::InvalidInput(_)));
    }

    #[test]
    fn write_tool_definition_has_correct_name() {
        let tool = WriteTool;
        let def = tool.definition();
        assert_eq!(def.name, "write");
    }

    // --- EditTool tests ---

    #[tokio::test]
    async fn edit_tool_happy_path() {
        let mut tmp = tempfile::NamedTempFile::new().expect("create temp file");
        write!(tmp, "hello world").expect("write temp file");
        let path = tmp.path().to_str().unwrap().to_owned();

        let tool = EditTool;
        let result = tool
            .execute(serde_json::json!({
                "path": path,
                "old_string": "world",
                "new_string": "rust"
            }))
            .await;
        assert!(result.is_ok(), "expected Ok, got: {:?}", result);

        let contents = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(contents, "hello rust");
    }

    #[tokio::test]
    async fn edit_tool_string_not_found() {
        let mut tmp = tempfile::NamedTempFile::new().expect("create temp file");
        write!(tmp, "hello world").expect("write temp file");
        let path = tmp.path().to_str().unwrap().to_owned();

        let tool = EditTool;
        let result = tool
            .execute(serde_json::json!({
                "path": path,
                "old_string": "nonexistent",
                "new_string": "replacement"
            }))
            .await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), Error::InvalidInput(_)));
    }

    #[tokio::test]
    async fn edit_tool_file_not_found() {
        let tool = EditTool;
        let result = tool
            .execute(serde_json::json!({
                "path": "/nonexistent/path/that/does/not/exist.txt",
                "old_string": "foo",
                "new_string": "bar"
            }))
            .await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), Error::InvalidInput(_)));
    }

    #[tokio::test]
    async fn edit_tool_replace_all() {
        let mut tmp = tempfile::NamedTempFile::new().expect("create temp file");
        write!(tmp, "foo foo foo").expect("write temp file");
        let path = tmp.path().to_str().unwrap().to_owned();

        let tool = EditTool;
        let result = tool
            .execute(serde_json::json!({
                "path": path,
                "old_string": "foo",
                "new_string": "bar",
                "replace_all": true
            }))
            .await;
        assert!(result.is_ok(), "expected Ok, got: {:?}", result);

        let contents = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(contents, "bar bar bar");
    }

    #[test]
    fn edit_tool_definition_has_correct_name() {
        let tool = EditTool;
        let def = tool.definition();
        assert_eq!(def.name, "edit");
    }
}
