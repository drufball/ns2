use crate::cwd::resolve_session_cwd_with_root;
use crate::history::{load_history, persist_user_message};
use crate::hooks::{run_post_tool_use_hooks, run_pre_tool_use_hooks, run_stop_hooks};
use crate::prompt::build_system_prompt;
use crate::retry::{complete_with_retry, is_rate_limit, max_retries};
use crate::HarnessConfig;
use chrono::Utc;
use events::{SessionEvent, StopEventStatus};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};
use tools::{StopSignal, StopStatus, StopTool};
use types::{ContentBlock, Role, SessionStatus, Turn};
use uuid::Uuid;

/// Run the tool dispatch loop for a single LLM turn sequence.
/// `messages` should already contain the full history including the new user message.
/// Hook subprocesses are started with `config.cwd` as their working directory so they
/// operate on the session's worktree rather than the server's cwd.
///
/// Returns the stop signal (if the agent called the `stop` tool) and an optional
/// injected message (if a Stop hook wants to re-enter the loop).
///
/// # Errors
///
/// Returns an error if the LLM call fails with a non-rate-limit error, or if any
/// database write fails.
#[allow(clippy::too_many_lines)]
#[allow(clippy::too_many_arguments)]
pub async fn run_tool_dispatch_loop(
    config: &HarnessConfig,
    client: &Arc<dyn anthropic::AnthropicClient>,
    db: &Arc<dyn db::Db>,
    event_tx: &broadcast::Sender<SessionEvent>,
    hooks: &agents::AgentHooks,
    system: Option<String>,
    mut messages: Vec<(Role, Vec<ContentBlock>)>,
    stop_rx: &mut mpsc::Receiver<StopSignal>,
) -> crate::Result<(Option<StopSignal>, Option<String>)> {
    let tool_definitions: Vec<types::ToolDefinition> =
        config.tools.iter().map(|t| t.definition()).collect();

    let mut stop_signal: Option<StopSignal> = None;

    loop {
        let request = anthropic::MessageRequest {
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
            token_count: Some(i64::from(response.input_tokens + response.output_tokens)),
            created_at: Utc::now(),
        };
        db.create_turn(&turn).await?;
        let _ = event_tx.send(SessionEvent::TurnStarted { turn: turn.clone() });

        // Store and emit content blocks
        for (index, block) in response.content.iter().enumerate() {
            let index = u32::try_from(index).unwrap_or(u32::MAX);
            if let ContentBlock::Text { text } = block {
                let _ = event_tx.send(SessionEvent::ContentBlockDelta {
                    turn_id: turn.id,
                    index,
                    delta: types::ContentBlockDelta::TextDelta { text: text.clone() },
                });
            }
            db.create_content_block(turn.id, i64::from(index), &Role::Assistant, block)
                .await?;
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
                // Check if a stop signal was sent during the preceding tool calls
                if let Ok(sig) = stop_rx.try_recv() {
                    stop_signal = Some(sig);
                }
                // Run Stop hooks; if any exit non-zero, return their stdout as an injected message
                if let Some(injected) =
                    run_stop_hooks(hooks, config.session.id, config.cwd.as_deref()).await
                {
                    return Ok((stop_signal, Some(injected)));
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
                    run_pre_tool_use_hooks(hooks, name, input, config.cwd.as_deref()).await
                {
                    // Hook blocked the tool — return hook stderr as the tool result
                    blocked
                } else {
                    // Run the actual tool
                    let tool_output =
                        match config.tools.iter().find(|t| t.definition().name == *name) {
                            Some(tool) => {
                                match tool.execute(input.clone(), config.cwd.as_deref()).await {
                                    Ok(output) => output,
                                    Err(e) => format!("Error: {e}"),
                                }
                            }
                            None => format!("Error: unknown tool '{name}'"),
                        };
                    // Run PostToolUse hooks (exit code ignored)
                    run_post_tool_use_hooks(
                        hooks,
                        name,
                        input,
                        &tool_output,
                        config.cwd.as_deref(),
                    )
                    .await;
                    tool_output
                };
                tool_result_blocks.push(ContentBlock::ToolResult {
                    tool_use_id: id.clone(),
                    content: result,
                });
            }
        }

        // After executing tools, drain the stop channel (the stop tool may have fired)
        if let Ok(sig) = stop_rx.try_recv() {
            stop_signal = Some(sig);
        }

        // Store tool result turn in DB
        let tool_result_turn = Turn {
            id: Uuid::new_v4(),
            session_id: config.session.id,
            token_count: None,
            created_at: Utc::now(),
        };
        db.create_turn(&tool_result_turn).await?;
        let _ = event_tx.send(SessionEvent::TurnStarted {
            turn: tool_result_turn.clone(),
        });

        for (index, block) in tool_result_blocks.iter().enumerate() {
            let index = u32::try_from(index).unwrap_or(u32::MAX);
            db.create_content_block(tool_result_turn.id, i64::from(index), &Role::User, block)
                .await?;
            let _ = event_tx.send(SessionEvent::ContentBlockDone {
                turn_id: tool_result_turn.id,
                index,
                block: block.clone(),
            });
        }

        let _ = event_tx.send(SessionEvent::TurnDone {
            turn_id: tool_result_turn.id,
        });

        // Add tool result turn to history and loop
        messages.push((Role::User, tool_result_blocks));
    }

    Ok((stop_signal, None))
}

/// # Errors
///
/// Returns an error if a database operation fails or the LLM call fails with a
/// non-rate-limit error.
pub async fn run(
    mut config: HarnessConfig,
    client: Arc<dyn anthropic::AnthropicClient>,
    db: Arc<dyn db::Db>,
    event_tx: broadcast::Sender<SessionEvent>,
    mut msg_rx: mpsc::Receiver<String>,
) -> crate::Result<()> {
    // Resolve the session's working directory once at startup.
    // If the session has an associated issue with a non-empty branch, create/reuse
    // a git worktree and set cwd to the worktree path.
    // Use config.git_root when provided (injected in tests); fall back to git_root().
    let session_git_root = if let Some(root) = config.git_root.clone() {
        Some(root)
    } else {
        workspace::git_root().await
    };
    let session_cwd = resolve_session_cwd_with_root(&db, config.session.id, session_git_root).await;

    // If we resolved a cwd, store it on config so it is passed to tool dispatch
    // and hook subprocesses at execution time.
    if let Some(cwd) = session_cwd {
        config.cwd = Some(cwd);
    }

    // Resolve the effective git root (injected in tests; discovered via git in production).
    let effective_root = if let Some(root) = config.git_root.clone() {
        Some(root)
    } else if let Some(cwd) = config.cwd.clone() {
        Some(cwd)
    } else {
        workspace::git_root().await
    };
    let agents_dir = effective_root
        .as_ref()
        .map(|r| r.join(".ns2").join("agents"));

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
                    let project_hooks = effective_root
                        .as_deref()
                        .map(agents::load_project_hooks)
                        .unwrap_or_default();
                    agents::merge_hooks(agent_hooks, project_hooks)
                } else {
                    agent_hooks
                }
            }
        }
    };

    // Create the stop channel. The StopTool is injected into config.tools so that
    // the agent can call it; we hold the receiver to read the signal after end_turn.
    let (mut stop_rx, stop_tool) = StopTool::new_pair();
    config.tools.push(Arc::new(stop_tool));

    loop {
        // Wait for the next user message. When the sender is dropped, recv() returns None → exit.
        let Some(message) = msg_rx.recv().await else {
            break;
        };

        // Transition session to Running
        db.update_session_status(config.session.id, SessionStatus::Running)
            .await?;

        // Persist user message and emit events
        persist_user_message(&db, config.session.id, &message, &event_tx).await?;

        // Load full conversation history from DB (including the message we just stored)
        let history = load_history(&db, config.session.id).await?;

        // Run the tool dispatch loop. It returns Some(injected_message) when a Stop
        // hook blocks completion and injects a message for the next turn.
        let mut current_history = history;
        let final_stop_signal = loop {
            match run_tool_dispatch_loop(
                &config,
                &client,
                &db,
                &event_tx,
                &hooks,
                system.clone(),
                current_history,
                &mut stop_rx,
            )
            .await?
            {
                (signal, None) => break signal, // normal completion
                (_, Some(injected)) => {
                    // Stop hook rejected; inject the message and continue
                    persist_user_message(&db, config.session.id, &injected, &event_tx).await?;
                    current_history = load_history(&db, config.session.id).await?;
                }
            }
        };

        // Determine the final session status from the stop signal (or default to Waiting).
        let final_status = match &final_stop_signal {
            Some(sig) if sig.status == StopStatus::Complete => SessionStatus::Completed,
            _ => SessionStatus::Waiting,
        };

        // Emit Stopped event before Done so issue watchers can act on it.
        if let Some(sig) = &final_stop_signal {
            let ev_status = match sig.status {
                StopStatus::Complete => StopEventStatus::Complete,
                StopStatus::Waiting => StopEventStatus::Waiting,
            };
            let _ = event_tx.send(SessionEvent::Stopped {
                status: ev_status,
                comment: sig.comment.clone(),
            });
        }

        // Mark session with final status and emit done event
        db.update_session_status(config.session.id, final_status)
            .await?;
        let _ = event_tx.send(SessionEvent::Done);

        // Loop back to wait for next message
    }

    Ok(())
}
