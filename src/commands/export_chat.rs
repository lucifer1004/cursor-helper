//! Export chat command - Export chat history to readable formats
//!
//! # Disclaimer
//!
//! This tool reads chat history from local SQLite databases stored on your machine
//! by the Cursor IDE. It accesses **your own data** for personal use, backup, and
//! data portability purposes.
//!
//! This tool does NOT:
//! - Reverse engineer, decompile, or modify Cursor's source code
//! - Access Cursor's cloud services or APIs
//! - Scrape data from Cursor's servers
//! - Create derivative works of Cursor itself
//!
//! The exported data belongs to you (the user). Please respect others' privacy
//! and do not share exported conversations without consent from all participants.
//!
//! This tool is not affiliated with or endorsed by Anysphere, Inc. (Cursor).

use anyhow::{bail, Context, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use super::utils;

/// Output format for chat export
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Markdown,
    Json,
}

impl ExportFormat {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "md" | "markdown" => Some(Self::Markdown),
            "json" => Some(Self::Json),
            _ => None,
        }
    }
}

/// Export options
#[derive(Debug, Clone, Default)]
pub struct ExportOptions {
    /// Include thinking/reasoning blocks
    pub with_thinking: bool,
    /// Include tool calls
    pub with_tools: bool,
    /// Include model info and token counts
    pub with_stats: bool,
    /// Include archived chat sessions
    pub include_archived: bool,
    /// Exclude sessions with no messages
    pub exclude_blank: bool,
}

/// Tool call information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// Tool name (e.g., "read_file_v2", "edit_file")
    pub name: String,
    /// Tool parameters
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<String>,
    /// Tool result (truncated for large outputs)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    /// Status: completed, failed, etc.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

/// A single message in a chat conversation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    /// Role: "user", "assistant", "tool", or "thinking"
    pub role: String,
    /// Message content
    pub content: String,
    /// Timestamp if available
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<i64>,
    /// Thinking duration in ms (for thinking messages)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_duration_ms: Option<i64>,
    /// Tool call info (for tool messages)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<ToolCall>,
    /// Model used (for assistant messages)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Token count
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<TokenCount>,
}

/// Token usage statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenCount {
    pub input: i64,
    pub output: i64,
}

/// A chat session/conversation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatSession {
    /// Session UUID
    pub id: String,
    /// Session title if available
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Messages in the conversation
    pub messages: Vec<ChatMessage>,
    /// When the session was created
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<i64>,
    /// When the session was last updated
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
}

/// Export result containing all chat sessions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatExport {
    /// Project path
    pub project_path: String,
    /// Export timestamp
    pub exported_at: i64,
    /// All chat sessions
    pub sessions: Vec<ChatSession>,
}

/// Execute the export-chat command
pub fn execute(
    project_path: &str,
    format: ExportFormat,
    output: Option<&str>,
    options: &ExportOptions,
    split: bool,
) -> Result<()> {
    let project_path = PathBuf::from(project_path);

    // Try to canonicalize for local paths, but allow remote paths that don't exist locally
    let (project_path, is_remote) = if project_path.exists() {
        let canonical = project_path
            .canonicalize()
            .with_context(|| format!("Failed to resolve: {}", project_path.display()))?;
        // On Windows, canonicalize() returns \\?\ prefix which we need to strip
        let canonical = utils::strip_windows_prefix(&canonical);
        (canonical, false)
    } else {
        // Path doesn't exist locally - might be a remote path
        // Make it absolute if it's relative
        let abs_path = if project_path.is_absolute() {
            project_path
        } else {
            std::env::current_dir()?.join(&project_path)
        };
        (abs_path, true)
    };

    // Find workspace storage for this project
    let workspace_dir = utils::find_workspace_dir(&project_path)?;

    let Some(workspace_dir) = workspace_dir else {
        if is_remote {
            bail!(
                "No Cursor workspace data found for remote path: {}\n\
                 Hint: For remote sessions, use the exact path as shown in Cursor\n\
                 (e.g., /home/user/project for SSH/tunnel connections)",
                project_path.display()
            );
        } else {
            bail!(
                "No Cursor workspace data found for: {}",
                project_path.display()
            );
        }
    };

    // Extract chat sessions
    let mut sessions = extract_chat_sessions(&workspace_dir, options)?;

    // Filter blank sessions if requested
    if options.exclude_blank {
        let before = sessions.len();
        sessions.retain(|s| !s.messages.is_empty());
        let filtered = before - sessions.len();
        if filtered > 0 {
            println!("Filtered {} blank session(s)", filtered);
        }
    }

    if sessions.is_empty() {
        println!("No chat sessions found for this project.");
        return Ok(());
    }

    println!("Found {} chat session(s)", sessions.len());

    let project_path_str = project_path.to_string_lossy().to_string();
    let exported_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    // Handle split output
    if split {
        let output_dir = output.ok_or_else(|| {
            anyhow::anyhow!("--split requires --output to specify the output directory")
        })?;

        write_split_output(&sessions, output_dir, format, &project_path_str, exported_at)?;
    } else {
        // Build single export
        let export = ChatExport {
            project_path: project_path_str,
            exported_at,
            sessions,
        };

        // Format output
        let content = match format {
            ExportFormat::Markdown => format_as_markdown(&export),
            ExportFormat::Json => serde_json::to_string_pretty(&export)?,
        };

        // Write or print
        if let Some(output_path) = output {
            fs::write(output_path, &content)
                .with_context(|| format!("Failed to write: {}", output_path))?;
            println!("Exported to: {}", output_path);
        } else {
            println!("{}", content);
        }
    }

    Ok(())
}

/// Execute the export-chat command using workspace ID directly
///
/// This is useful for remote sessions where the path doesn't exist locally.
/// Use `cursor-helper list` to find workspace IDs.
pub fn execute_by_id(
    workspace_id: &str,
    format: ExportFormat,
    output: Option<&str>,
    options: &ExportOptions,
    split: bool,
) -> Result<()> {
    let workspace_storage_dir = crate::config::workspace_storage_dir()?;
    let workspace_dir = workspace_storage_dir.join(workspace_id);

    if !workspace_dir.exists() {
        bail!(
            "Workspace not found: {}\n\
             Hint: Use 'cursor-helper list' to see available workspaces",
            workspace_id
        );
    }

    // Try to read the project path from workspace.json
    let project_path = {
        let workspace_json = workspace_dir.join("workspace.json");
        if workspace_json.exists() {
            let content = fs::read_to_string(&workspace_json)?;
            let ws: serde_json::Value = serde_json::from_str(&content)?;
            ws.get("folder")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("workspace:{}", workspace_id))
        } else {
            format!("workspace:{}", workspace_id)
        }
    };

    // Extract chat sessions
    let mut sessions = extract_chat_sessions(&workspace_dir, options)?;

    // Filter blank sessions if requested
    if options.exclude_blank {
        let before = sessions.len();
        sessions.retain(|s| !s.messages.is_empty());
        let filtered = before - sessions.len();
        if filtered > 0 {
            println!("Filtered {} blank session(s)", filtered);
        }
    }

    if sessions.is_empty() {
        println!("No chat sessions found for this workspace.");
        return Ok(());
    }

    println!("Found {} chat session(s)", sessions.len());

    let exported_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    // Handle split output
    if split {
        let output_dir = output.ok_or_else(|| {
            anyhow::anyhow!("--split requires --output to specify the output directory")
        })?;

        write_split_output(&sessions, output_dir, format, &project_path, exported_at)?;
    } else {
        // Build single export
        let export = ChatExport {
            project_path,
            exported_at,
            sessions,
        };

        // Format output
        let content = match format {
            ExportFormat::Markdown => format_as_markdown(&export),
            ExportFormat::Json => serde_json::to_string_pretty(&export)?,
        };

        // Write or print
        if let Some(output_path) = output {
            fs::write(output_path, &content)
                .with_context(|| format!("Failed to write: {}", output_path))?;
            println!("Exported to: {}", output_path);
        } else {
            println!("{}", content);
        }
    }

    Ok(())
}

/// Extract chat sessions from a workspace directory
fn extract_chat_sessions(
    workspace_dir: &Path,
    options: &ExportOptions,
) -> Result<Vec<ChatSession>> {
    let db_path = workspace_dir.join("state.vscdb");

    if !db_path.exists() {
        return Ok(vec![]);
    }

    let conn = Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("Failed to open: {}", db_path.display()))?;

    // Query composer metadata from workspace storage
    let composer_data: Option<String> = conn
        .query_row(
            "SELECT value FROM ItemTable WHERE key = 'composer.composerData'",
            [],
            |row| row.get(0),
        )
        .ok();

    // Parse composer metadata to get session info
    let composers: Vec<ComposerInfo> = composer_data
        .as_ref()
        .and_then(|data| parse_composer_data(data, options.include_archived))
        .unwrap_or_default();

    if composers.is_empty() {
        return Ok(vec![]);
    }

    // Open global storage for bubble content
    let global_db_path = crate::config::global_storage_dir()
        .ok()
        .map(|d| d.join("state.vscdb"))
        .filter(|p| p.exists());

    let global_conn = global_db_path.and_then(|path| {
        Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .ok()
    });

    // Build sessions with messages from global storage
    let mut sessions = Vec::new();

    for composer in composers {
        let messages = if let Some(ref gconn) = global_conn {
            fetch_session_messages(gconn, &composer.composer_id, options).unwrap_or_default()
        } else {
            vec![]
        };

        sessions.push(ChatSession {
            id: composer.composer_id.clone(),
            title: Some(composer.name.clone()),
            messages,
            created_at: Some(composer.created_at / 1000),
            updated_at: Some(composer.last_updated_at / 1000),
        });
    }

    // Sort by creation time (newest first)
    sessions.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    Ok(sessions)
}

/// Fetch messages for a session from global storage
fn fetch_session_messages(
    conn: &Connection,
    composer_id: &str,
    options: &ExportOptions,
) -> Result<Vec<ChatMessage>> {
    let composer_key = format!("composerData:{}", composer_id);

    // Get composer data (stored as TEXT in cursorDiskKV)
    let composer_str: String = match conn.query_row(
        "SELECT value FROM cursorDiskKV WHERE key = ?1",
        rusqlite::params![&composer_key],
        |row| row.get::<_, String>(0),
    ) {
        Ok(s) => s,
        Err(_) => return Ok(vec![]), // Session not found in global storage
    };

    let composer_data: serde_json::Value = serde_json::from_str(&composer_str)?;

    // Get bubble headers (bubbleId + type)
    let Some(headers) = composer_data
        .get("fullConversationHeadersOnly")
        .and_then(|v| v.as_array())
    else {
        return Ok(vec![]);
    };

    let mut messages = Vec::new();

    for header in headers {
        let Some(bubble_id) = header.get("bubbleId").and_then(|v| v.as_str()) else {
            continue;
        };
        let bubble_type = header.get("type").and_then(|v| v.as_i64()).unwrap_or(0);

        // Fetch bubble content
        let bubble_key = format!("bubbleId:{}:{}", composer_id, bubble_id);
        let bubble_str: Option<String> = conn
            .query_row(
                "SELECT value FROM cursorDiskKV WHERE key = ?",
                [&bubble_key],
                |row| row.get(0),
            )
            .ok();

        if let Some(json_str) = bubble_str {
            if let Ok(bubble) = serde_json::from_str::<serde_json::Value>(&json_str) {
                // Parse timestamp from ISO string
                let timestamp = bubble
                    .get("createdAt")
                    .and_then(|v| v.as_str())
                    .and_then(parse_iso_timestamp);

                // Check for thinking block (capabilityType=30 with thinking field)
                if options.with_thinking {
                    if let Some(thinking) = bubble.get("thinking").and_then(|t| t.as_object()) {
                        if let Some(thinking_text) = thinking.get("text").and_then(|v| v.as_str()) {
                            if !thinking_text.is_empty() {
                                let thinking_duration =
                                    bubble.get("thinkingDurationMs").and_then(|v| v.as_i64());

                                messages.push(ChatMessage {
                                    role: "thinking".to_string(),
                                    content: thinking_text.to_string(),
                                    timestamp,
                                    thinking_duration_ms: thinking_duration,
                                    tool_call: None,
                                    model: None,
                                    tokens: None,
                                });
                            }
                        }
                    }
                }

                // Check for tool call (capabilityType=15 with toolFormerData)
                if options.with_tools {
                    if let Some(tool_data) =
                        bubble.get("toolFormerData").and_then(|t| t.as_object())
                    {
                        let tool_name = tool_data
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown")
                            .to_string();

                        let params = tool_data
                            .get("params")
                            .and_then(|v| v.as_str())
                            .map(|s| truncate_str(s, 500));

                        let result = tool_data
                            .get("result")
                            .and_then(|v| v.as_str())
                            .map(|s| truncate_str(s, 1000));

                        let status = tool_data
                            .get("status")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());

                        messages.push(ChatMessage {
                            role: "tool".to_string(),
                            content: format!("[{}]", tool_name),
                            timestamp,
                            thinking_duration_ms: None,
                            tool_call: Some(ToolCall {
                                name: tool_name,
                                params,
                                result,
                                status,
                            }),
                            model: None,
                            tokens: None,
                        });

                        continue; // Tool calls don't have regular text content
                    }
                }

                // Regular message content
                let text = bubble
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                if !text.is_empty() {
                    let role = match bubble_type {
                        1 => "user",
                        2 => "assistant",
                        _ => "unknown",
                    };

                    // Extract model info and tokens if requested
                    let model = if options.with_stats {
                        bubble
                            .get("modelInfo")
                            .and_then(|m| m.get("modelName"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                    } else {
                        None
                    };

                    let tokens = if options.with_stats {
                        bubble.get("tokenCount").and_then(|tc| {
                            let input = tc.get("inputTokens").and_then(|v| v.as_i64())?;
                            let output = tc.get("outputTokens").and_then(|v| v.as_i64())?;
                            if input > 0 || output > 0 {
                                Some(TokenCount { input, output })
                            } else {
                                None
                            }
                        })
                    } else {
                        None
                    };

                    messages.push(ChatMessage {
                        role: role.to_string(),
                        content: text,
                        timestamp,
                        thinking_duration_ms: None,
                        tool_call: None,
                        model,
                        tokens,
                    });
                }
            }
        }
    }

    Ok(messages)
}

/// Parse ISO 8601 timestamp to Unix timestamp
fn parse_iso_timestamp(s: &str) -> Option<i64> {
    // Simple parsing for "2026-01-19T04:31:31.394Z" format
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp())
}

/// Truncate string to max length (char-safe)
fn truncate_str(s: &str, max_chars: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{}...[truncated]", truncated)
    }
}

/// Composer metadata from composer.composerData
#[derive(Debug, Clone)]
struct ComposerInfo {
    composer_id: String,
    name: String,
    created_at: i64,
    last_updated_at: i64,
}

/// Parse composer.composerData JSON
fn parse_composer_data(data: &str, include_archived: bool) -> Option<Vec<ComposerInfo>> {
    let json: serde_json::Value = serde_json::from_str(data).ok()?;
    let composers = json.get("allComposers")?.as_array()?;

    let mut result = Vec::new();
    for c in composers {
        // Skip archived composers unless explicitly included
        let is_archived = c
            .get("isArchived")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if is_archived && !include_archived {
            continue;
        }

        // Try to parse each composer, skip if any required field is missing
        let Some(composer_id) = c.get("composerId").and_then(|v| v.as_str()) else {
            continue;
        };
        let name = c
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("Untitled")
            .to_string();
        let Some(created_at) = c.get("createdAt").and_then(|v| v.as_i64()) else {
            continue;
        };
        let last_updated_at = c
            .get("lastUpdatedAt")
            .and_then(|v| v.as_i64())
            .unwrap_or(created_at);

        result.push(ComposerInfo {
            composer_id: composer_id.to_string(),
            name,
            created_at,
            last_updated_at,
        });
    }

    Some(result)
}

/// Format export as markdown
fn format_as_markdown(export: &ChatExport) -> String {
    let mut md = String::new();

    md.push_str(&format!("# Chat Export: {}\n\n", export.project_path));
    md.push_str(&format!(
        "_Exported: {}_\n\n",
        format_timestamp(export.exported_at)
    ));
    md.push_str("---\n\n");

    for (i, session) in export.sessions.iter().enumerate() {
        let title = session.title.as_deref().unwrap_or("Untitled Session");

        md.push_str(&format!("## Session {}: {}\n\n", i + 1, title));

        if let Some(created) = session.created_at {
            md.push_str(&format!("_Created: {}_\n\n", format_timestamp(created)));
        }

        for msg in &session.messages {
            match msg.role.as_str() {
                "thinking" => {
                    md.push_str("### 💭 **Thinking**");
                    if let Some(duration) = msg.thinking_duration_ms {
                        md.push_str(&format!(" _{:.1}s_", duration as f64 / 1000.0));
                    }
                    md.push_str("\n\n");
                    md.push_str("<details>\n<summary>Click to expand thinking...</summary>\n\n");
                    md.push_str(&msg.content);
                    md.push_str("\n\n</details>\n\n");
                }
                "tool" => {
                    if let Some(ref tc) = msg.tool_call {
                        md.push_str(&format!("### 🔧 **Tool: {}**", tc.name));
                        if let Some(ref status) = tc.status {
                            md.push_str(&format!(" [{}]", status));
                        }
                        md.push_str("\n\n");

                        if let Some(ref params) = tc.params {
                            md.push_str("<details>\n<summary>Parameters</summary>\n\n```json\n");
                            md.push_str(params);
                            md.push_str("\n```\n\n</details>\n\n");
                        }

                        if let Some(ref result) = tc.result {
                            md.push_str("<details>\n<summary>Result</summary>\n\n```\n");
                            md.push_str(result);
                            md.push_str("\n```\n\n</details>\n\n");
                        }
                    }
                }
                _ => {
                    let role_display = match msg.role.as_str() {
                        "user" => "**User**",
                        "assistant" => "**Assistant**",
                        "system" => "**System**",
                        other => other,
                    };

                    md.push_str(&format!("### {}", role_display));

                    // Add model info if present
                    if let Some(ref model) = msg.model {
                        md.push_str(&format!(" _{}_", model));
                    }

                    // Add token count if present
                    if let Some(ref tokens) = msg.tokens {
                        if tokens.input > 0 || tokens.output > 0 {
                            md.push_str(&format!(" ({}↓ {}↑)", tokens.input, tokens.output));
                        }
                    }

                    md.push_str("\n\n");
                    md.push_str(&msg.content);
                    md.push_str("\n\n");
                }
            }
        }

        md.push_str("---\n\n");
    }

    md
}

/// Write sessions to separate files in a directory
fn write_split_output(
    sessions: &[ChatSession],
    output_dir: &str,
    format: ExportFormat,
    project_path: &str,
    exported_at: i64,
) -> Result<()> {
    // Create output directory
    fs::create_dir_all(output_dir)
        .with_context(|| format!("Failed to create directory: {}", output_dir))?;

    let ext = match format {
        ExportFormat::Markdown => "md",
        ExportFormat::Json => "json",
    };

    for (i, session) in sessions.iter().enumerate() {
        let title = session.title.as_deref().unwrap_or("Untitled");
        let safe_title = sanitize_filename(title);
        let filename = format!("{:03}-{}.{}", i + 1, safe_title, ext);
        let file_path = Path::new(output_dir).join(&filename);

        let content = match format {
            ExportFormat::Markdown => format_single_session_as_markdown(session, i + 1),
            ExportFormat::Json => {
                let single_export = ChatExport {
                    project_path: project_path.to_string(),
                    exported_at,
                    sessions: vec![session.clone()],
                };
                serde_json::to_string_pretty(&single_export)?
            }
        };

        fs::write(&file_path, &content)
            .with_context(|| format!("Failed to write: {}", file_path.display()))?;
    }

    println!(
        "Exported {} sessions to directory: {}",
        sessions.len(),
        output_dir
    );

    Ok(())
}

/// Sanitize a string for use as a filename
fn sanitize_filename(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .take(50) // Limit filename length
        .collect::<String>()
        .trim()
        .to_string()
}

/// Format a single session as markdown (for split output)
fn format_single_session_as_markdown(session: &ChatSession, index: usize) -> String {
    let mut md = String::new();

    let title = session.title.as_deref().unwrap_or("Untitled Session");
    md.push_str(&format!("# Session {}: {}\n\n", index, title));

    if let Some(created) = session.created_at {
        md.push_str(&format!("_Created: {}_\n\n", format_timestamp(created)));
    }

    md.push_str("---\n\n");

    for msg in &session.messages {
        match msg.role.as_str() {
            "thinking" => {
                md.push_str("## 💭 **Thinking**");
                if let Some(duration) = msg.thinking_duration_ms {
                    md.push_str(&format!(" _{:.1}s_", duration as f64 / 1000.0));
                }
                md.push_str("\n\n");
                md.push_str("<details>\n<summary>Click to expand thinking...</summary>\n\n");
                md.push_str(&msg.content);
                md.push_str("\n\n</details>\n\n");
            }
            "tool" => {
                if let Some(ref tc) = msg.tool_call {
                    md.push_str(&format!("## 🔧 **Tool: {}**", tc.name));
                    if let Some(ref status) = tc.status {
                        md.push_str(&format!(" [{}]", status));
                    }
                    md.push_str("\n\n");

                    if let Some(ref params) = tc.params {
                        md.push_str("<details>\n<summary>Parameters</summary>\n\n```json\n");
                        md.push_str(params);
                        md.push_str("\n```\n\n</details>\n\n");
                    }

                    if let Some(ref result) = tc.result {
                        md.push_str("<details>\n<summary>Result</summary>\n\n```\n");
                        md.push_str(result);
                        md.push_str("\n```\n\n</details>\n\n");
                    }
                }
            }
            _ => {
                let role_display = match msg.role.as_str() {
                    "user" => "**User**",
                    "assistant" => "**Assistant**",
                    "system" => "**System**",
                    other => other,
                };

                md.push_str(&format!("## {}", role_display));

                // Add model info if present
                if let Some(ref model) = msg.model {
                    md.push_str(&format!(" _{}_", model));
                }

                // Add token count if present
                if let Some(ref tokens) = msg.tokens {
                    if tokens.input > 0 || tokens.output > 0 {
                        md.push_str(&format!(" ({}↓ {}↑)", tokens.input, tokens.output));
                    }
                }

                md.push_str("\n\n");
                md.push_str(&msg.content);
                md.push_str("\n\n");
            }
        }
    }

    md
}

/// Format unix timestamp as human-readable string
fn format_timestamp(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|| ts.to_string())
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_export_format() {
        assert_eq!(ExportFormat::from_str("md"), Some(ExportFormat::Markdown));
        assert_eq!(
            ExportFormat::from_str("markdown"),
            Some(ExportFormat::Markdown)
        );
        assert_eq!(ExportFormat::from_str("json"), Some(ExportFormat::Json));
        assert_eq!(ExportFormat::from_str("xml"), None);
    }

}
