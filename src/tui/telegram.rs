//! Optional Telegram bot integration for remote permission approval.
//!
//! When `TELEGRAM_BOT_TOKEN` and `TELEGRAM_CHAT_ID` environment variables are
//! set, the TUI forwards permission requests to a Telegram chat with inline
//! Approve / Deny buttons.  Callback responses are routed back through an
//! `mpsc` channel to resolve the permission in the main event loop.
//!
//! If the env vars are absent the feature is completely dormant.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use serde_json::Value;

// ---------------------------------------------------------------------------
// Channel message types
// ---------------------------------------------------------------------------

/// Message sent from the TUI event loop **to** the Telegram thread.
pub enum TgOutbound {
    /// Forward a new permission request to the Telegram chat.
    NewPermission {
        perm_id: u64,
        task_name: String,
        tool_name: String,
        tool_input_summary: String,
    },
    /// A permission was resolved; edit the Telegram message to reflect the
    /// outcome (e.g. "Approved locally", "Denied via Telegram", …).
    Resolved { perm_id: u64, outcome: String },
    /// Stream an ExO response chunk (accumulate in the bot thread).
    ExoTextDelta { text: String },
    /// ExO finished responding — send the accumulated message.
    ExoTurnDone,
}

/// How the user responded to a permission request via Telegram.
#[derive(Debug, PartialEq)]
pub enum PermAction {
    Approve,
    Trust,
    Deny,
}

/// Message sent from the Telegram thread **to** the TUI event loop.
pub enum TgInbound {
    /// The user tapped an inline button in Telegram.
    PermissionDecision { perm_id: u64, action: PermAction },
    /// The user sent a text message in Telegram (route to ExO).
    ExoMessage { text: String },
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Start the Telegram bot in a background thread.
///
/// Returns `(sender, receiver)` channels for bidirectional communication.
/// The thread shuts down when `cancel` is set to `true`.
pub fn start(
    token: String,
    chat_id: String,
    cancel: Arc<AtomicBool>,
) -> (mpsc::Sender<TgOutbound>, mpsc::Receiver<TgInbound>) {
    let (out_tx, out_rx) = mpsc::channel();
    let (in_tx, in_rx) = mpsc::channel();

    std::thread::spawn(move || {
        run_bot(&token, &chat_id, cancel, out_rx, in_tx);
    });

    (out_tx, in_rx)
}

// ---------------------------------------------------------------------------
// Bot loop (runs in its own thread)
// ---------------------------------------------------------------------------

/// Shared context passed through the bot's helper functions to avoid
/// exceeding clippy's max-argument limit.
struct BotCtx {
    agent: ureq::Agent,
    base: String,
    file_base: String,
    chat_id: String,
    /// Path to the whisper GGML model file (for voice transcription).
    whisper_model: String,
}

fn run_bot(
    token: &str,
    chat_id: &str,
    cancel: Arc<AtomicBool>,
    out_rx: mpsc::Receiver<TgOutbound>,
    in_tx: mpsc::Sender<TgInbound>,
) {
    tg_log(&format!(
        "Bot starting (chat_id=***, token_len={})",
        token.len()
    ));
    let ctx = BotCtx {
        agent: ureq::AgentBuilder::new()
            .timeout_read(Duration::from_secs(5))
            .timeout_write(Duration::from_secs(5))
            .build(),
        base: format!("https://api.telegram.org/bot{token}"),
        file_base: format!("https://api.telegram.org/file/bot{token}"),
        chat_id: chat_id.to_string(),
        whisper_model: std::env::var("WHISPER_MODEL")
            .unwrap_or_else(|_| "data/ggml-base.bin".to_string()),
    };
    let mut offset: i64 = 0;
    // perm_id → Telegram message_id so we can edit messages later.
    let mut msg_map: HashMap<u64, i64> = HashMap::new();

    // Buffer for accumulating streamed ExO response text.
    let mut exo_buf = String::new();

    while !cancel.load(Ordering::Relaxed) {
        // 1. Drain outbound messages from the TUI (non-blocking).
        drain_outbound(&ctx, &out_rx, &mut msg_map, &mut exo_buf);

        // 2. Long-poll Telegram for updates.
        poll_updates(&ctx, &mut offset, &in_tx, &mut msg_map);
    }
}

/// Process all queued outbound messages without blocking.
fn drain_outbound(
    ctx: &BotCtx,
    out_rx: &mpsc::Receiver<TgOutbound>,
    msg_map: &mut HashMap<u64, i64>,
    exo_buf: &mut String,
) {
    while let Ok(msg) = out_rx.try_recv() {
        match msg {
            TgOutbound::NewPermission {
                perm_id,
                task_name,
                tool_name,
                tool_input_summary,
            } => {
                let text = format_perm_text(&task_name, &tool_name, &tool_input_summary);
                let body = serde_json::json!({
                    "chat_id": ctx.chat_id,
                    "text": text,
                    "parse_mode": "HTML",
                    "reply_markup": {
                        "inline_keyboard": [[
                            {"text": "✅ Approve", "callback_data": format!("a:{perm_id}")},
                            {"text": "🔓 Trust",   "callback_data": format!("t:{perm_id}")},
                            {"text": "❌ Deny",    "callback_data": format!("d:{perm_id}")},
                        ]]
                    },
                });
                if let Some(resp) = tg_post(&ctx.agent, &format!("{}/sendMessage", ctx.base), &body)
                    && let Some(id) = resp["result"]["message_id"].as_i64()
                {
                    msg_map.insert(perm_id, id);
                }
            }
            TgOutbound::Resolved { perm_id, outcome } => {
                if let Some(msg_id) = msg_map.remove(&perm_id) {
                    let body = serde_json::json!({
                        "chat_id": ctx.chat_id,
                        "message_id": msg_id,
                        "text": outcome,
                        "parse_mode": "HTML",
                    });
                    tg_post(&ctx.agent, &format!("{}/editMessageText", ctx.base), &body);
                }
            }
            TgOutbound::ExoTextDelta { text } => {
                exo_buf.push_str(&text);
            }
            TgOutbound::ExoTurnDone => {
                if !exo_buf.is_empty() {
                    let body = serde_json::json!({
                        "chat_id": ctx.chat_id,
                        "text": exo_buf.as_str(),
                    });
                    tg_post(&ctx.agent, &format!("{}/sendMessage", ctx.base), &body);
                    exo_buf.clear();
                }
            }
        }
    }
}

/// Long-poll Telegram for updates and forward callback queries to the TUI.
fn poll_updates(
    ctx: &BotCtx,
    offset: &mut i64,
    in_tx: &mpsc::Sender<TgInbound>,
    msg_map: &mut HashMap<u64, i64>,
) {
    let body = serde_json::json!({
        "offset": *offset,
        "timeout": 2,
        "allowed_updates": ["callback_query", "message"],
    });
    let Some(resp) = tg_post(&ctx.agent, &format!("{}/getUpdates", ctx.base), &body) else {
        return;
    };
    let Some(updates) = resp["result"].as_array() else {
        return;
    };
    for update in updates {
        if let Some(uid) = update["update_id"].as_i64() {
            *offset = uid + 1;
        }
        // Handle callback queries (permission buttons).
        if let Some(cb) = update.get("callback_query") {
            // Answer the callback query to dismiss Telegram's loading spinner.
            if let Some(cb_id) = cb["id"].as_str() {
                let answer = serde_json::json!({"callback_query_id": cb_id});
                tg_post(
                    &ctx.agent,
                    &format!("{}/answerCallbackQuery", ctx.base),
                    &answer,
                );
            }
            // Parse the decision and forward to the TUI.
            if let Some(data) = cb["data"].as_str()
                && let Some((perm_id, action)) = parse_callback(data)
            {
                let label = match &action {
                    PermAction::Approve => "✅ <b>Approved</b> via Telegram",
                    PermAction::Trust => "🔓 <b>Trusted</b> via Telegram",
                    PermAction::Deny => "❌ <b>Denied</b> via Telegram",
                };
                let _ = in_tx.send(TgInbound::PermissionDecision { perm_id, action });
                // Edit the message to show the result immediately.
                if let Some(msg_id) = msg_map.remove(&perm_id) {
                    let body = serde_json::json!({
                        "chat_id": ctx.chat_id,
                        "message_id": msg_id,
                        "text": label,
                        "parse_mode": "HTML",
                    });
                    tg_post(&ctx.agent, &format!("{}/editMessageText", ctx.base), &body);
                }
            }
        }

        // Handle regular text messages → route to ExO.
        if let Some(msg) = update.get("message")
            && let Some(msg_chat_id) = msg["chat"]["id"].as_i64()
            && msg_chat_id.to_string() == ctx.chat_id
            && let Some(text) = msg["text"].as_str()
        {
            tg_log(&format!("Incoming message for ExO: {text}"));
            let _ = in_tx.send(TgInbound::ExoMessage {
                text: text.to_string(),
            });
        }

        // Handle voice messages → transcribe and route to ExO.
        if let Some(msg) = update.get("message")
            && let Some(msg_chat_id) = msg["chat"]["id"].as_i64()
            && msg_chat_id.to_string() == ctx.chat_id
            && msg.get("voice").is_some()
            && let Some(file_id) = msg["voice"]["file_id"].as_str()
        {
            handle_voice(ctx, file_id, in_tx);
        }
    }
}

// ---------------------------------------------------------------------------
// Telegram API helpers
// ---------------------------------------------------------------------------

/// JSON POST with file logging for debugging.
fn tg_post(agent: &ureq::Agent, url: &str, body: &Value) -> Option<Value> {
    let method = url.rsplit('/').next().unwrap_or("?");
    tg_log(&format!(">>> {method}\n    body: {body}"));
    match agent.post(url).send_json(body) {
        Ok(resp) => match resp.into_json::<Value>() {
            Ok(val) => {
                tg_log(&format!("<<< OK: {val}"));
                Some(val)
            }
            Err(e) => {
                tg_log(&format!("<<< parse error: {e}"));
                None
            }
        },
        Err(ureq::Error::Status(code, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            tg_log(&format!("<<< HTTP {code}: {body}"));
            None
        }
        Err(e) => {
            tg_log(&format!("<<< transport error: {e}"));
            None
        }
    }
}

fn tg_log(msg: &str) {
    let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("data/telegram.log")
    else {
        return;
    };
    let ts = chrono::Local::now().format("%H:%M:%S%.3f");
    let _ = writeln!(f, "[{ts}] {msg}");
}

// ---------------------------------------------------------------------------
// Voice message handling
// ---------------------------------------------------------------------------

/// Process an incoming voice message: download, transcribe locally, forward to ExO.
fn handle_voice(ctx: &BotCtx, file_id: &str, in_tx: &mpsc::Sender<TgInbound>) {
    let Some(audio_data) = download_voice(&ctx.agent, &ctx.base, &ctx.file_base, file_id) else {
        tg_send(ctx, "⚠️ Failed to download voice message.");
        return;
    };

    // Ensure the whisper model is available (auto-download on first use).
    if !ensure_whisper_model(ctx) {
        return;
    }

    match transcribe_audio(&ctx.whisper_model, &audio_data) {
        Some(text) if !text.is_empty() => {
            tg_log(&format!("Transcribed voice: {text}"));
            tg_send(ctx, &format!("🎤 {text}"));
            let _ = in_tx.send(TgInbound::ExoMessage { text });
        }
        _ => {
            tg_send(ctx, "⚠️ Failed to transcribe voice message.");
        }
    }
}

/// Download a voice file from Telegram using the Bot File API.
///
/// 1. Call `getFile` to resolve the `file_id` to a `file_path`.
/// 2. GET the file bytes from `https://api.telegram.org/file/bot<token>/<file_path>`.
fn download_voice(
    agent: &ureq::Agent,
    base: &str,
    file_base: &str,
    file_id: &str,
) -> Option<Vec<u8>> {
    let body = serde_json::json!({"file_id": file_id});
    let resp = tg_post(agent, &format!("{base}/getFile"), &body)?;
    let file_path = resp["result"]["file_path"].as_str()?;

    let url = format!("{file_base}/{file_path}");
    tg_log(&format!(">>> downloading voice: {url}"));
    match agent.get(&url).call() {
        Ok(resp) => {
            let mut buf = Vec::new();
            if resp.into_reader().read_to_end(&mut buf).is_ok() {
                tg_log(&format!("<<< downloaded {} bytes", buf.len()));
                Some(buf)
            } else {
                tg_log("<<< failed to read voice response body");
                None
            }
        }
        Err(e) => {
            tg_log(&format!("<<< voice download error: {e}"));
            None
        }
    }
}

/// Ensure the whisper GGML model file exists, downloading it if necessary.
///
/// Uses the HuggingFace-hosted ggml-base.bin (~142 MB) and streams it directly
/// to disk so we don't buffer the whole file in memory.
fn ensure_whisper_model(ctx: &BotCtx) -> bool {
    use std::path::Path;

    if Path::new(&ctx.whisper_model).exists() {
        return true;
    }

    tg_log("Whisper model not found, downloading ggml-base.bin …");
    tg_send(ctx, "⏳ Downloading whisper model (first time only)…");

    let url = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin";
    let dl_agent = ureq::AgentBuilder::new()
        .timeout_read(Duration::from_secs(300))
        .build();

    match dl_agent.get(url).call() {
        Ok(resp) => {
            // Ensure parent directory exists.
            if let Some(parent) = Path::new(&ctx.whisper_model).parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let mut file = match std::fs::File::create(&ctx.whisper_model) {
                Ok(f) => f,
                Err(e) => {
                    tg_log(&format!("Failed to create model file: {e}"));
                    tg_send(ctx, "⚠️ Failed to save whisper model.");
                    return false;
                }
            };
            if let Err(e) = std::io::copy(&mut resp.into_reader(), &mut file) {
                tg_log(&format!("Failed to write model file: {e}"));
                let _ = std::fs::remove_file(&ctx.whisper_model);
                tg_send(ctx, "⚠️ Failed to download whisper model.");
                return false;
            }
            tg_log("Whisper model downloaded successfully");
            true
        }
        Err(e) => {
            tg_log(&format!("Model download error: {e}"));
            tg_send(ctx, "⚠️ Failed to download whisper model.");
            false
        }
    }
}

/// Transcribe audio using the local `whisper-cli` binary.
///
/// Telegram sends OGG/Opus which whisper-cli can't decode directly,
/// so we convert to WAV via ffmpeg first.
fn transcribe_audio(model_path: &str, audio_data: &[u8]) -> Option<String> {
    use std::process::Command;

    let pid = std::process::id();
    let ogg_tmp = std::env::temp_dir().join(format!("voice-{pid}.ogg"));
    let wav_tmp = std::env::temp_dir().join(format!("voice-{pid}.wav"));

    if std::fs::write(&ogg_tmp, audio_data).is_err() {
        tg_log("Failed to write temp audio file");
        return None;
    }

    // Convert OGG/Opus → WAV (16kHz mono, whisper's expected format).
    tg_log(&format!(
        ">>> ffmpeg converting OGG→WAV ({} bytes)",
        audio_data.len()
    ));
    let ffmpeg = Command::new("ffmpeg")
        .args(["-y", "-i"])
        .arg(&ogg_tmp)
        .args(["-ar", "16000", "-ac", "1", "-f", "wav"])
        .arg(&wav_tmp)
        .output();

    let _ = std::fs::remove_file(&ogg_tmp);

    match &ffmpeg {
        Ok(out) if !out.status.success() => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            tg_log(&format!("<<< ffmpeg failed: {stderr}"));
            return None;
        }
        Err(e) => {
            tg_log(&format!("<<< ffmpeg spawn error: {e}"));
            return None;
        }
        _ => {}
    }

    tg_log(">>> whisper-cli transcription");

    let result = Command::new("whisper-cli")
        .args([
            "-m", model_path, "-nt", // no timestamps
            "-np", // no extra prints
            "-l", "auto", // auto-detect language
        ])
        .arg(&wav_tmp)
        .output();

    let _ = std::fs::remove_file(&wav_tmp);

    match result {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
            tg_log(&format!("<<< whisper-cli output: {text}"));
            if text.is_empty() { None } else { Some(text) }
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tg_log(&format!("<<< whisper-cli failed: {stderr}"));
            None
        }
        Err(e) => {
            tg_log(&format!("<<< whisper-cli spawn error: {e}"));
            None
        }
    }
}

/// Convenience: send a plain text message to the bot's chat.
fn tg_send(ctx: &BotCtx, text: &str) {
    let body = serde_json::json!({
        "chat_id": ctx.chat_id,
        "text": text,
    });
    tg_post(&ctx.agent, &format!("{}/sendMessage", ctx.base), &body);
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

fn format_perm_text(task_name: &str, tool_name: &str, summary: &str) -> String {
    let detail = if summary.is_empty() {
        format!("<code>{tool_name}</code>")
    } else {
        format!("<code>{tool_name}</code>: <code>{summary}</code>")
    };
    format!("🔔 <b>Permission Request</b>\n\nTask: <code>{task_name}</code>\n{detail}")
}

fn parse_callback(data: &str) -> Option<(u64, PermAction)> {
    let (prefix, id_str) = data.split_once(':')?;
    let perm_id: u64 = id_str.parse().ok()?;
    match prefix {
        "a" => Some((perm_id, PermAction::Approve)),
        "t" => Some((perm_id, PermAction::Trust)),
        "d" => Some((perm_id, PermAction::Deny)),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_callback_approve() {
        assert_eq!(parse_callback("a:42"), Some((42, PermAction::Approve)));
    }

    #[test]
    fn parse_callback_trust() {
        assert_eq!(parse_callback("t:5"), Some((5, PermAction::Trust)));
    }

    #[test]
    fn parse_callback_deny() {
        assert_eq!(parse_callback("d:7"), Some((7, PermAction::Deny)));
    }

    #[test]
    fn parse_callback_invalid_prefix() {
        assert_eq!(parse_callback("x:1"), None);
    }

    #[test]
    fn parse_callback_non_numeric_id() {
        assert_eq!(parse_callback("a:abc"), None);
    }

    #[test]
    fn parse_callback_no_colon() {
        assert_eq!(parse_callback("nocolon"), None);
    }

    #[test]
    fn format_perm_text_with_summary() {
        let text = format_perm_text("fix-bug", "Bash", "cargo test");
        assert!(text.contains("fix-bug"));
        assert!(text.contains("cargo test"));
        assert!(text.contains("Permission Request"));
    }

    #[test]
    fn format_perm_text_empty_summary() {
        let text = format_perm_text("fix-bug", "Agent", "");
        assert!(text.contains("Agent"));
        assert!(!text.contains(": <code></code>"));
    }
}
