use crate::hooks::{run_post_tool_use_hooks, run_pre_tool_use_hooks, run_stop_hooks};
use crate::prompt::build_system_prompt;
use crate::HarnessConfig;
use anthropic::{AnthropicClient, MessageRequest, MessageResponse};
use chrono::Utc;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};
use types::{ContentBlock, ContentBlockDelta, Role, SessionEvent, SessionStatus, Turn};
use uuid::Uuid;

// ── 429 retry logic ──────────────────────────────────────────────────────────

/// Read the max-retry count from `NS2_MAX_RETRIES` (default 5).
pub(crate) fn max_retries() -> u32 {
    std::env::var("NS2_MAX_RETRIES")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(5)
}

/// Returns `true` if the error is an Anthropic 429 rate-limit response.
pub(crate) fn is_rate_limit(err: &anthropic::Error) -> bool {
    matches!(err, anthropic::Error::Api { status: 429, .. })
}

/// Call `client.complete(request)` with exponential-backoff retry on 429.
///
/// - Retries up to `NS2_MAX_RETRIES` times (default 5).
/// - Initial delay: 10 s, doubling each retry, capped at 120 s.
/// - Non-429 errors propagate immediately without retrying.
///
/// In tests using `#[tokio::test(start_paused = true)]`, `tokio::time::sleep`
/// returns instantly, so no real wall-clock time is consumed.
pub(crate) async fn complete_with_retry(
    client: &Arc<dyn AnthropicClient>,
    request: MessageRequest,
) -> anthropic::Result<MessageResponse> {
    use tokio::time::{sleep, Duration};

    let retries = max_retries();
    let mut delay_ms: u64 = 10_000;
    const MAX_DELAY_MS: u64 = 120_000;

    let mut attempt = 0u32;
    loop {
        match client.complete(request.clone()).await {
            Ok(resp) => return Ok(resp),
            Err(err) if is_rate_limit(&err) && attempt < retries => {
                attempt += 1;
                tracing::warn!(
                    attempt,
                    retries,
                    delay_ms,
                    "Anthropic 429 rate-limit; retrying after {delay_ms}ms"
                );
                sleep(Duration::from_millis(delay_ms)).await;
                delay_ms = (delay_ms * 2).min(MAX_DELAY_MS);
            }
            Err(err) => return Err(err),
        }
    }
}

// ── Worktree management ───────────────────────────────────────────────────────

/// Resolve the session's working directory based on its associated issue.
///
/// - If the session has an associated issue with a non-empty `branch`:
///   reads `ns2.toml`, computes `<base>/<branch>`, ensures the worktree
///   exists, and returns the worktree path.
/// - Otherwise: returns `None` (use git root as cwd).
pub async fn resolve_session_cwd(
    db: &Arc<dyn db::Db>,
    session_id: Uuid,
) -> Option<PathBuf> {
    resolve_session_cwd_with_root(db, session_id, workspace::git_root().await).await
}

/// Inner implementation that accepts an explicit `git_root` — injectable for tests.
pub(crate) async fn resolve_session_cwd_with_root(
    db: &Arc<dyn db::Db>,
    session_id: Uuid,
    git_root: Option<PathBuf>,
) -> Option<PathBuf> {
    let issues = db.list_issues_by_session_id(session_id).await.ok()?;
    let branch = issues.into_iter().find_map(|i| {
        if i.branch.is_empty() { None } else { Some(i.branch) }
    })?;

    let git_root = git_root?;
    let config = workspace::read_ns2_config(&git_root);
    let worktree_path = config.worktree_base.join(&branch);

    workspace::ensure_worktree(&git_root, &worktree_path, &branch).await
}

// ── Context (history) ─────────────────────────────────────────────────────────

/// Load conversation history from the DB for a session.
/// Returns turns in order, each as `(Role, Vec<ContentBlock>)`.
/// Turns with mixed roles are grouped by the role stored on each block;
/// consecutive blocks with the same role are merged into one entry.
pub(crate) async fn load_history(
    db: &Arc<dyn db::Db>,
    session_id: Uuid,
) -> crate::Result<Vec<(Role, Vec<ContentBlock>)>> {
    let turns = db.list_turns(session_id).await?;
    let mut history: Vec<(Role, Vec<ContentBlock>)> = Vec::new();

    for turn in &turns {
        let blocks = db.list_content_blocks(turn.id).await?;
        if blocks.is_empty() {
            continue;
        }
        // Each turn is stored with a consistent role; group all blocks under one entry.
        // If blocks have mixed roles (shouldn't happen in practice), group by first role.
        let role = blocks[0].0.clone();
        let content: Vec<ContentBlock> = blocks.into_iter().map(|(_, b)| b).collect();

        // Merge with previous entry if same role
        if let Some(last) = history.last_mut() {
            if last.0 == role {
                last.1.extend(content);
                continue;
            }
        }
        history.push((role, content));
    }

    Ok(history)
}

/// Persist a user message as a turn+block in the DB and emit events.
pub(crate) async fn persist_user_message(
    db: &Arc<dyn db::Db>,
    session_id: Uuid,
    message: &str,
    event_tx: &broadcast::Sender<SessionEvent>,
) -> crate::Result<Turn> {
    let user_turn = Turn {
        id: Uuid::new_v4(),
        session_id,
        token_count: None,
        created_at: Utc::now(),
    };
    db.create_turn(&user_turn).await?;
    let user_block = ContentBlock::Text { text: message.to_string() };
    db.create_content_block(user_turn.id, 0, &Role::User, &user_block).await?;
    let _ = event_tx.send(SessionEvent::TurnStarted { turn: user_turn.clone() });
    let _ = event_tx.send(SessionEvent::ContentBlockDelta {
        turn_id: user_turn.id,
        index: 0,
        delta: ContentBlockDelta::TextDelta { text: message.to_string() },
    });
    let _ = event_tx.send(SessionEvent::ContentBlockDone {
        turn_id: user_turn.id,
        index: 0,
        block: user_block,
    });
    let _ = event_tx.send(SessionEvent::TurnDone { turn_id: user_turn.id });
    Ok(user_turn)
}

// ── Tool dispatch loop ────────────────────────────────────────────────────────

/// Run the tool dispatch loop for a single LLM turn sequence.
/// `messages` should already contain the full history including the new user message.
pub(crate) async fn run_tool_dispatch_loop(
    config: &HarnessConfig,
    client: &Arc<dyn AnthropicClient>,
    db: &Arc<dyn db::Db>,
    event_tx: &broadcast::Sender<SessionEvent>,
    hooks: &agents::AgentHooks,
    system: Option<String>,
    mut messages: Vec<(Role, Vec<ContentBlock>)>,
) -> crate::Result<Option<String>> {
    let tool_definitions: Vec<types::ToolDefinition> =
        config.tools.iter().map(|t| t.definition()).collect();

    loop {
        let request = MessageRequest {
            model: config.model.clone(),
            system: system.clone(),
            messages: messages.clone(),
            max_tokens: 64000,
            tools: tool_definitions.clone(),
        };

        let response = match complete_with_retry(client, request).await {
            Ok(r) => r,
            Err(err) => {
                // Retries exhausted (429) or non-retryable error → emit error event
                // and break out of the dispatch loop. Non-429 errors are re-raised
                // to the caller so the harness task can fail clearly.
                if is_rate_limit(&err) {
                    let _ = event_tx.send(SessionEvent::Error {
                        message: format!(
                            "rate limited after {retries} retries: {err}",
                            retries = max_retries()
                        ),
                    });
                    break;
                }
                return Err(crate::Error::Anthropic(err));
            }
        };

        // Create assistant turn in DB
        let turn = Turn {
            id: Uuid::new_v4(),
            session_id: config.session.id,
            token_count: Some((response.input_tokens + response.output_tokens) as i64),
            created_at: Utc::now(),
        };
        db.create_turn(&turn).await?;
        let _ = event_tx.send(SessionEvent::TurnStarted { turn: turn.clone() });

        // Store and emit content blocks
        for (index, block) in response.content.iter().enumerate() {
            let index = index as u32;
            if let ContentBlock::Text { text } = block {
                let _ = event_tx.send(SessionEvent::ContentBlockDelta {
                    turn_id: turn.id,
                    index,
                    delta: ContentBlockDelta::TextDelta { text: text.clone() },
                });
            }
            db.create_content_block(turn.id, index as i64, &Role::Assistant, block).await?;
            let _ = event_tx.send(SessionEvent::ContentBlockDone {
                turn_id: turn.id,
                index,
                block: block.clone(),
            });
        }

        let _ = event_tx.send(SessionEvent::TurnDone { turn_id: turn.id });

        match response.stop_reason.as_str() {
            "tool_use" => { /* dispatch tools below */ }
            "end_turn" => {
                // Run Stop hooks; if any exit non-zero, return their stdout as an injected message
                if let Some(injected) = run_stop_hooks(hooks, config.session.id).await {
                    return Ok(Some(injected));
                }
                break;
            }
            "max_tokens" => {
                let _ = event_tx.send(SessionEvent::Error {
                    message: "session hit max_tokens limit".to_string(),
                });
                break;
            }
            other => {
                tracing::warn!("unknown stop_reason: {other}");
                break;
            }
        }

        // Add assistant turn to history
        messages.push((Role::Assistant, response.content.clone()));

        // Execute tool calls and build tool result turn
        let mut tool_result_blocks: Vec<ContentBlock> = Vec::new();

        for block in &response.content {
            if let ContentBlock::ToolUse { id, name, input } = block {
                // Run PreToolUse hooks
                let result = if let Some(blocked) =
                    run_pre_tool_use_hooks(hooks, name, input).await
                {
                    // Hook blocked the tool — return hook stderr as the tool result
                    blocked
                } else {
                    // Run the actual tool
                    let tool_output = match config.tools.iter().find(|t| t.definition().name == *name) {
                        Some(tool) => match tool.execute(input.clone()).await {
                            Ok(output) => output,
                            Err(e) => format!("Error: {e}"),
                        },
                        None => format!("Error: unknown tool '{name}'"),
                    };
                    // Run PostToolUse hooks (exit code ignored)
                    run_post_tool_use_hooks(hooks, name, input, &tool_output).await;
                    tool_output
                };
                tool_result_blocks.push(ContentBlock::ToolResult {
                    tool_use_id: id.clone(),
                    content: result,
                });
            }
        }

        // Store tool result turn in DB
        let tool_result_turn = Turn {
            id: Uuid::new_v4(),
            session_id: config.session.id,
            token_count: None,
            created_at: Utc::now(),
        };
        db.create_turn(&tool_result_turn).await?;
        let _ = event_tx.send(SessionEvent::TurnStarted { turn: tool_result_turn.clone() });

        for (index, block) in tool_result_blocks.iter().enumerate() {
            let index = index as u32;
            db.create_content_block(tool_result_turn.id, index as i64, &Role::User, block)
                .await?;
            let _ = event_tx.send(SessionEvent::ContentBlockDone {
                turn_id: tool_result_turn.id,
                index,
                block: block.clone(),
            });
        }

        let _ = event_tx.send(SessionEvent::TurnDone { turn_id: tool_result_turn.id });

        // Add tool result turn to history and loop
        messages.push((Role::User, tool_result_blocks));
    }

    Ok(None)
}

// ── Main run loop ─────────────────────────────────────────────────────────────

pub async fn run(
    mut config: HarnessConfig,
    client: Arc<dyn AnthropicClient>,
    db: Arc<dyn db::Db>,
    event_tx: broadcast::Sender<SessionEvent>,
    mut msg_rx: mpsc::Receiver<String>,
) -> crate::Result<()> {
    // Resolve the session's working directory once at startup.
    // If the session has an associated issue with a non-empty branch, create/reuse
    // a git worktree and set cwd to the worktree path.
    let session_cwd = resolve_session_cwd(&db, config.session.id).await;

    // If we resolved a cwd, rebuild the standard tool set with that cwd so all
    // file operations and shell commands run relative to the worktree.
    if let Some(ref cwd) = session_cwd {
        config.tools = vec![
            Arc::new(tools::BashTool { cwd: Some(cwd.clone()) }),
            Arc::new(tools::ReadTool { cwd: Some(cwd.clone()) }),
            Arc::new(tools::WriteTool { cwd: Some(cwd.clone()) }),
            Arc::new(tools::EditTool { cwd: Some(cwd.clone()) }),
        ];
    }

    // Resolve the effective git root (injected in tests; discovered via git in production).
    let effective_root = if let Some(root) = config.git_root.clone() {
        Some(root)
    } else {
        workspace::git_root().await
    };
    let agents_dir = effective_root.as_ref().map(|r| r.join(".ns2").join("agents"));

    // Build a preamble that tells the agent where it is running.
    // If the git root cannot be determined, the preamble is omitted (no failure).
    // Pre-compute the system prompt once (it does not change across turns).
    // The preamble (if available) is always prepended before the agent body so
    // every agent session knows its working directory and repository name.
    let system: Option<String> = build_system_prompt(
        effective_root.as_deref(),
        agents_dir.as_deref(),
        config.session.agent.as_deref(),
    );

    // Load hooks from the agent definition (once, at harness start)
    // When include_project_config is true, also load project hooks from
    // .claude/settings.json and merge them (agent hooks take precedence).
    let hooks = {
        let agent_def = config.session.agent.as_deref().and_then(|name| {
            let dir = agents_dir.as_ref()?;
            agents::load_agent(dir, name)
        });

        match agent_def {
            None => agents::AgentHooks::default(),
            Some(def) => {
                let agent_hooks = def.hooks;
                if def.include_project_config {
                    let project_hooks = effective_root.as_deref()
                        .map(agents::load_project_hooks)
                        .unwrap_or_default();
                    agents::merge_hooks(agent_hooks, project_hooks)
                } else {
                    agent_hooks
                }
            }
        }
    };

    loop {
        // Wait for the next user message. When the sender is dropped, recv() returns None → exit.
        let message = match msg_rx.recv().await {
            Some(m) => m,
            None => break,
        };

        // Transition session to Running
        db.update_session_status(config.session.id, SessionStatus::Running).await?;

        // Persist user message and emit events
        persist_user_message(&db, config.session.id, &message, &event_tx).await?;

        // Load full conversation history from DB (including the message we just stored)
        let history = load_history(&db, config.session.id).await?;

        // Run the tool dispatch loop. It returns Some(injected_message) when a Stop
        // hook blocks completion and injects a message for the next turn.
        let mut current_history = history;
        loop {
            match run_tool_dispatch_loop(
                &config,
                &client,
                &db,
                &event_tx,
                &hooks,
                system.clone(),
                current_history,
            )
            .await?
            {
                None => break, // normal completion
                Some(injected) => {
                    // Stop hook rejected; inject the message and continue
                    persist_user_message(
                        &db,
                        config.session.id,
                        &injected,
                        &event_tx,
                    )
                    .await?;
                    current_history = load_history(&db, config.session.id).await?;
                }
            }
        }

        // Mark session completed and emit done event
        db.update_session_status(config.session.id, SessionStatus::Completed).await?;
        let _ = event_tx.send(SessionEvent::SessionDone { session_id: config.session.id });

        // Loop back to wait for next message
    }

    Ok(())
}
