//! Local MCP server. Speaks JSON-RPC 2.0 over stdio (one message per line) and
//! forwards a small set of download tools to the authenticated Okytes daemon.
//! stdout carries protocol messages only; diagnostics go to stderr.
//!
//! The server holds no engine of its own. Grant it a permission-restricted
//! token with `okytes-cli plugin-grant apps/mcp` so the daemon, not this
//! process, decides what an assistant may do.
use okytes_core::desktop::{Action, DesktopService, Job, Submit};
use okytes_daemon_api::Client;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const PROTOCOLS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];
const MAX_LINE: usize = 1024 * 1024;

fn connect() -> Result<Client, String> {
    let url = std::env::var("OKYTES_DAEMON_URL")
        .map_err(|_| "Set OKYTES_DAEMON_URL to the Okytes daemon address.".to_owned())?;
    let file = std::env::var_os("OKYTES_PLUGIN_TOKEN_FILE")
        .or_else(|| std::env::var_os("OKYTES_DAEMON_TOKEN_FILE"))
        .ok_or_else(|| {
            "Set OKYTES_PLUGIN_TOKEN_FILE to a token from `okytes-cli plugin-grant`.".to_owned()
        })?;
    Client::from_file(url, std::path::Path::new(&file))
        .map_err(|_| "The daemon URL or token file is invalid.".to_owned())
}

// Signed URLs carry credentials in their query; assistants see origin and path only.
fn redact(url: &str) -> String {
    let url = url.trim_start_matches("hls+").trim_start_matches("dash+");
    url.split(['?', '#']).next().unwrap_or_default().to_owned()
}

fn summary(job: &Job) -> Value {
    json!({
        "id": job.id,
        "name": job.name,
        "state": job.state,
        "source": redact(&job.url),
        "destination": job.destination,
        "bytes": job.bytes,
        "total": job.total,
        "percent": job.total.filter(|t| *t > 0.0).map(|t| (job.bytes / t * 1000.0).round() / 10.0),
        "speed_bytes_per_second": job.speed,
        "retries": job.retries,
        "error": job.error,
    })
}

fn tools() -> Value {
    let id = json!({
        "type": "object",
        "properties": { "id": { "type": "string", "description": "Download ID from list_downloads" } },
        "required": ["id"],
        "additionalProperties": false
    });
    json!([
        {
            "name": "list_downloads",
            "description": "List Okytes downloads (newest history page) with state and progress.",
            "inputSchema": {
                "type": "object",
                "properties": { "offset": { "type": "integer", "minimum": 0, "description": "History offset" } },
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "get_download",
            "description": "Get one download's state, progress, retries and last error.",
            "inputSchema": id,
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "add_download",
            "description": "Start downloading an HTTP(S), FTP or SFTP URL into the user's Okytes download folder.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "Direct file URL" },
                    "filename": { "type": "string", "description": "Optional file name, without folders" }
                },
                "required": ["url"],
                "additionalProperties": false
            }
        },
        { "name": "pause_download", "description": "Pause a download, keeping progress.", "inputSchema": id },
        { "name": "resume_download", "description": "Resume a paused or failed download.", "inputSchema": id },
        {
            "name": "cancel_download",
            "description": "Cancel a download and delete its partial data. Completed files are kept.",
            "inputSchema": id,
            "annotations": { "destructiveHint": true }
        }
    ])
}

fn text(arguments: &Value, key: &str) -> Result<String, String> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("`{key}` is required"))
}

async fn call(client: &Client, name: &str, arguments: &Value) -> Result<Value, String> {
    let fail = |e: okytes_core::error::EngineError| e.to_string();
    match name {
        "list_downloads" => {
            let offset = arguments.get("offset").and_then(Value::as_u64).unwrap_or(0);
            let page = client
                .list(u32::try_from(offset).map_err(|_| "offset is too large")?)
                .await
                .map_err(fail)?;
            Ok(json!({
                "downloads": page.jobs.iter().map(summary).collect::<Vec<_>>(),
                "next_offset": page.next,
            }))
        }
        "get_download" => Ok(summary(
            &client.get(text(arguments, "id")?).await.map_err(fail)?,
        )),
        "add_download" => {
            let id = client
                .submit(Submit {
                    credentials: None,
                    url: text(arguments, "url")?,
                    destination: None,
                    filename: arguments
                        .get("filename")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    sha256: None,
                    conflict: "rename".into(),
                })
                .await
                .map_err(fail)?;
            Ok(json!({ "id": id, "status": "queued" }))
        }
        "pause_download" | "resume_download" | "cancel_download" => {
            let action = match name {
                "pause_download" => Action::Pause,
                "resume_download" => Action::Resume,
                _ => Action::Cancel,
            };
            let id = text(arguments, "id")?;
            client.control(id.clone(), action).await.map_err(fail)?;
            Ok(json!({ "id": id, "ok": true }))
        }
        _ => Err(format!("unknown tool `{name}`")),
    }
}

async fn handle(client: &Result<Client, String>, message: &Value) -> Option<Value> {
    let id = message.get("id").cloned();
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    // Notifications (no id) never receive a response.
    let id = id?;
    let result = match method {
        "initialize" => {
            let requested = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let version = PROTOCOLS
                .iter()
                .find(|v| **v == requested)
                .copied()
                .unwrap_or(PROTOCOLS[0]);
            Ok(json!({
                "protocolVersion": version,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "okytes", "version": env!("CARGO_PKG_VERSION") },
                "instructions": "Manage downloads in the user's local Okytes download manager. Confirm with the user before cancelling downloads."
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
            // Tool failures are results the model can read, not protocol errors.
            let outcome = match client {
                Ok(client) => call(client, name, &arguments).await,
                Err(message) => Err(message.clone()),
            };
            Ok(match outcome {
                Ok(value) => json!({
                    "content": [{ "type": "text", "text": value.to_string() }],
                    "structuredContent": value,
                    "isError": false
                }),
                Err(error) => {
                    eprintln!("okytes-mcp: {name} failed: {error}");
                    json!({ "content": [{ "type": "text", "text": error }], "isError": true })
                }
            })
        }
        _ => Err(json!({ "code": -32601, "message": "Method not found" })),
    };
    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error }),
    })
}

#[tokio::main]
async fn main() {
    let client = connect();
    if let Err(message) = &client {
        eprintln!("okytes-mcp: {message}");
    }
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let reply = if line.len() > MAX_LINE {
            Some(
                json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32600, "message": "Message too large" } }),
            )
        } else {
            match serde_json::from_str::<Value>(&line) {
                Ok(Value::Array(_)) => Some(
                    json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32600, "message": "Batches are not supported" } }),
                ),
                Ok(message) => handle(&client, &message).await,
                Err(_) => Some(
                    json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": "Parse error" } }),
                ),
            }
        };
        if let Some(reply) = reply {
            let mut bytes = reply.to_string().into_bytes();
            bytes.push(b'\n');
            if stdout.write_all(&bytes).await.is_err() || stdout.flush().await.is_err() {
                break;
            }
        }
    }
}
