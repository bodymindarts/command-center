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
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

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
    /// Forward an AskUserQuestion prompt with inline buttons for each option.
    NewQuestion {
        perm_id: u64,
        task_name: String,
        question: String,
        /// (label, description) pairs for each option.
        options: Vec<(String, String)>,
    },
    /// A permission was resolved; edit the Telegram message to reflect the
    /// outcome (e.g. "Approved locally", "Denied via Telegram", …).
    Resolved { perm_id: u64, outcome: String },
    /// Send a one-shot notification message (no buttons).
    Notify { text: String },
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
    /// The user selected an option from an AskUserQuestion prompt.
    QuestionAnswer { perm_id: u64, answer: String },
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
///
/// The bot runs inside a supervisor loop that catches panics and restarts
/// automatically after a brief delay.  This makes the integration resilient
/// to unexpected errors (e.g. transient network failures that trigger a
/// panic inside `ureq`).
pub fn start(
    token: String,
    chat_id: String,
    cancel: Arc<AtomicBool>,
) -> (
    mpsc::UnboundedSender<TgOutbound>,
    mpsc::UnboundedReceiver<TgInbound>,
) {
    let (out_tx, out_rx) = mpsc::unbounded_channel();
    let (in_tx, in_rx) = mpsc::unbounded_channel();

    std::thread::spawn(move || {
        supervisor_loop(token, chat_id, cancel, out_rx, in_tx);
    });

    (out_tx, in_rx)
}

/// Supervisor that (re)starts [`run_bot`] whenever it exits unexpectedly or
/// panics.  Waits 5 seconds between restarts to avoid tight crash-loops.
fn supervisor_loop(
    token: String,
    chat_id: String,
    cancel: Arc<AtomicBool>,
    mut out_rx: mpsc::UnboundedReceiver<TgOutbound>,
    in_tx: mpsc::UnboundedSender<TgInbound>,
) {
    while !cancel.load(Ordering::Relaxed) {
        // `AssertUnwindSafe` is acceptable here: the mpsc channels are
        // internally synchronised and safe to reuse after an unwind.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_bot(&token, &chat_id, &cancel, &mut out_rx, &in_tx);
        }));

        if cancel.load(Ordering::Relaxed) {
            break;
        }

        match result {
            Ok(()) => {
                tg_log("Bot loop exited unexpectedly, restarting in 5s…");
            }
            Err(payload) => {
                let msg = panic_payload_message(&payload);
                tg_log(&format!("PANIC in bot thread: {msg} — restarting in 5s…"));
            }
        }

        sleep_cancelable(&cancel, Duration::from_secs(5));
    }
    tg_log("Supervisor exiting (cancel requested)");
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

/// Tracks state for AskUserQuestion messages so callbacks can resolve labels.
struct QuestionState {
    /// perm_id → list of option labels (index matches callback data).
    labels: HashMap<u64, Vec<String>>,
}

fn run_bot(
    token: &str,
    chat_id: &str,
    cancel: &AtomicBool,
    out_rx: &mut mpsc::UnboundedReceiver<TgOutbound>,
    in_tx: &mpsc::UnboundedSender<TgInbound>,
) {
    tg_log(&format!(
        "Bot starting (chat_id=***, token_len={})",
        token.len()
    ));
    let ctx = BotCtx {
        agent: ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout_read(Duration::from_secs(10))
            .timeout_write(Duration::from_secs(5))
            // Hard wall-clock cap for the entire request lifecycle.
            // Prevents the bot thread from hanging indefinitely on a
            // dead TCP socket in half-open state where SO_RCVTIMEO
            // doesn't fire.
            .timeout(Duration::from_secs(30))
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

    // Track AskUserQuestion option labels for callback resolution.
    let mut questions = QuestionState {
        labels: HashMap::new(),
    };

    // Consecutive poll failures — drives exponential backoff.
    let mut consecutive_errors: u32 = 0;

    while !cancel.load(Ordering::Relaxed) {
        // Back off on consecutive errors to avoid hammering a dead connection.
        if consecutive_errors > 0 {
            // 2, 4, 8, 16, 30, 30, … seconds
            let secs = std::cmp::min(1u64 << consecutive_errors.min(5), 30);
            if consecutive_errors <= 3 || consecutive_errors.is_multiple_of(10) {
                // Log the first few backoffs and then every 10th to reduce noise.
                tg_log(&format!(
                    "Backing off {secs}s (consecutive errors: {consecutive_errors})"
                ));
            }
            sleep_cancelable(cancel, Duration::from_secs(secs));
            if cancel.load(Ordering::Relaxed) {
                break;
            }
        }

        // 1. Drain outbound messages from the TUI (non-blocking).
        drain_outbound(&ctx, out_rx, &mut msg_map, &mut exo_buf, &mut questions);

        // 2. Long-poll Telegram for updates.
        let ok = poll_updates(&ctx, &mut offset, in_tx, &mut msg_map, &mut questions);
        if ok {
            if consecutive_errors > 0 {
                tg_log(&format!(
                    "Reconnected after {consecutive_errors} consecutive errors"
                ));
            }
            consecutive_errors = 0;
        } else {
            consecutive_errors = consecutive_errors.saturating_add(1);
        }
    }
}

/// Process all queued outbound messages without blocking.
fn drain_outbound(
    ctx: &BotCtx,
    out_rx: &mut mpsc::UnboundedReceiver<TgOutbound>,
    msg_map: &mut HashMap<u64, i64>,
    exo_buf: &mut String,
    questions: &mut QuestionState,
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
                match tg_post(&ctx.agent, &format!("{}/sendMessage", ctx.base), &body) {
                    Some(resp) if resp["result"]["message_id"].as_i64().is_some() => {
                        msg_map.insert(perm_id, resp["result"]["message_id"].as_i64().unwrap());
                    }
                    Some(resp) => {
                        tg_log(&format!(
                            "WARN: permission {perm_id} ({task_name}): sent OK but no message_id in response: {resp}"
                        ));
                    }
                    None => {
                        // tg_post already logged the HTTP/transport error.
                        tg_log(&format!(
                            "WARN: permission {perm_id} ({task_name}): sendMessage failed (see error above)"
                        ));
                    }
                }
            }
            TgOutbound::NewQuestion {
                perm_id,
                task_name,
                question,
                options,
            } => {
                let text = format_question_text(&task_name, &question, &options);
                let buttons: Vec<Value> = options
                    .iter()
                    .enumerate()
                    .map(|(i, (label, _))| {
                        serde_json::json!({
                            "text": label,
                            "callback_data": format!("q:{perm_id}:{i}"),
                        })
                    })
                    .collect();
                // Store labels for callback resolution.
                questions
                    .labels
                    .insert(perm_id, options.iter().map(|(l, _)| l.clone()).collect());
                let body = serde_json::json!({
                    "chat_id": ctx.chat_id,
                    "text": text,
                    "parse_mode": "HTML",
                    "reply_markup": {
                        "inline_keyboard": buttons.iter().map(|b| vec![b.clone()]).collect::<Vec<_>>(),
                    },
                });
                match tg_post(&ctx.agent, &format!("{}/sendMessage", ctx.base), &body) {
                    Some(resp) if resp["result"]["message_id"].as_i64().is_some() => {
                        msg_map.insert(perm_id, resp["result"]["message_id"].as_i64().unwrap());
                    }
                    Some(resp) => {
                        tg_log(&format!(
                            "WARN: question {perm_id} ({task_name}): sent OK but no message_id in response: {resp}"
                        ));
                    }
                    None => {
                        tg_log(&format!(
                            "WARN: question {perm_id} ({task_name}): sendMessage failed (see error above)"
                        ));
                    }
                }
            }
            TgOutbound::Notify { text } => {
                tg_send(ctx, &text);
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
                questions.labels.remove(&perm_id);
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
///
/// Returns `true` on success (even if there are zero updates), `false` on
/// transport or API error (drives the backoff logic in [`run_bot`]).
fn poll_updates(
    ctx: &BotCtx,
    offset: &mut i64,
    in_tx: &mpsc::UnboundedSender<TgInbound>,
    msg_map: &mut HashMap<u64, i64>,
    questions: &mut QuestionState,
) -> bool {
    let body = serde_json::json!({
        "offset": *offset,
        "timeout": 2,
        "allowed_updates": ["callback_query", "message"],
    });
    let Some(resp) = tg_post(&ctx.agent, &format!("{}/getUpdates", ctx.base), &body) else {
        return false;
    };
    let Some(updates) = resp["result"].as_array() else {
        return false;
    };
    for update in updates {
        if let Some(uid) = update["update_id"].as_i64() {
            *offset = uid + 1;
        }
        // Handle callback queries (permission buttons + question answers).
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
            if let Some(data) = cb["data"].as_str() {
                // Try permission callback first (a:/t:/d: prefixes).
                if let Some((perm_id, action)) = parse_callback(data) {
                    let label = match &action {
                        PermAction::Approve => "✅ <b>Approved</b> via Telegram",
                        PermAction::Trust => "🔓 <b>Trusted</b> via Telegram",
                        PermAction::Deny => "❌ <b>Denied</b> via Telegram",
                    };
                    let _ = in_tx.send(TgInbound::PermissionDecision { perm_id, action });
                    if let Some(msg_id) = msg_map.remove(&perm_id) {
                        let body = serde_json::json!({
                            "chat_id": ctx.chat_id,
                            "message_id": msg_id,
                            "text": label,
                            "parse_mode": "HTML",
                        });
                        tg_post(&ctx.agent, &format!("{}/editMessageText", ctx.base), &body);
                    }
                // Try question callback (q: prefix).
                } else if let Some((perm_id, idx)) = parse_question_callback(data) {
                    let answer = questions
                        .labels
                        .get(&perm_id)
                        .and_then(|labels| labels.get(idx))
                        .cloned()
                        .unwrap_or_else(|| format!("Option {idx}"));
                    let _ = in_tx.send(TgInbound::QuestionAnswer {
                        perm_id,
                        answer: answer.clone(),
                    });
                    // Edit message to show the selected answer.
                    if let Some(msg_id) = msg_map.remove(&perm_id) {
                        let body = serde_json::json!({
                            "chat_id": ctx.chat_id,
                            "message_id": msg_id,
                            "text": format!("✅ Selected: <b>{answer}</b>"),
                            "parse_mode": "HTML",
                        });
                        tg_post(&ctx.agent, &format!("{}/editMessageText", ctx.base), &body);
                    }
                    questions.labels.remove(&perm_id);
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
    true
}

// ---------------------------------------------------------------------------
// Helpers: sleep, panic extraction
// ---------------------------------------------------------------------------

/// Sleep for `duration` in small increments, returning early if `cancel` is set.
fn sleep_cancelable(cancel: &AtomicBool, duration: Duration) {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline && !cancel.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Extract a human-readable message from a `catch_unwind` panic payload.
fn panic_payload_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
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

pub(super) fn tg_log(msg: &str) {
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
fn handle_voice(ctx: &BotCtx, file_id: &str, in_tx: &mpsc::UnboundedSender<TgInbound>) {
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

/// Maximum length for the tool_input_summary sent to Telegram.
const MAX_SUMMARY_LEN: usize = 300;

/// Escape characters that are special in Telegram's HTML parse mode.
fn html_escape(s: &str) -> String {
    minijinja::HtmlEscape(s).to_string()
}

/// Truncate a string to at most `max` characters, appending "…" if truncated.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut end = max;
        // Avoid splitting a multi-byte character.
        while !s.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        format!("{}…", &s[..end])
    }
}

fn format_question_text(task_name: &str, question: &str, options: &[(String, String)]) -> String {
    let task_name = html_escape(task_name);
    let question = html_escape(question);
    let mut text = format!("❓ <b>Question</b>\n\nTask: <code>{task_name}</code>\n\n{question}\n");
    for (label, desc) in options {
        let label = html_escape(label);
        let desc = html_escape(desc);
        if desc.is_empty() {
            text.push_str(&format!("\n• <b>{label}</b>"));
        } else {
            text.push_str(&format!("\n• <b>{label}</b> — {desc}"));
        }
    }
    text
}

fn format_perm_text(task_name: &str, tool_name: &str, summary: &str) -> String {
    let task_name = html_escape(task_name);
    let tool_name = html_escape(tool_name);
    let summary = truncate(&html_escape(summary), MAX_SUMMARY_LEN);
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

/// Parse a question answer callback: `q:<perm_id>:<option_index>`.
fn parse_question_callback(data: &str) -> Option<(u64, usize)> {
    let rest = data.strip_prefix("q:")?;
    let (id_str, idx_str) = rest.split_once(':')?;
    let perm_id: u64 = id_str.parse().ok()?;
    let idx: usize = idx_str.parse().ok()?;
    Some((perm_id, idx))
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
    fn parse_question_callback_valid() {
        assert_eq!(parse_question_callback("q:42:0"), Some((42, 0)));
        assert_eq!(parse_question_callback("q:1:3"), Some((1, 3)));
    }

    #[test]
    fn parse_question_callback_invalid() {
        assert_eq!(parse_question_callback("a:42"), None);
        assert_eq!(parse_question_callback("q:42"), None);
        assert_eq!(parse_question_callback("q:abc:0"), None);
        assert_eq!(parse_question_callback("q:1:xyz"), None);
    }

    #[test]
    fn format_question_text_with_descriptions() {
        let options = vec![
            ("React".to_string(), "Popular UI library".to_string()),
            ("Vue".to_string(), "Progressive framework".to_string()),
        ];
        let text = format_question_text("my-task", "Which library?", &options);
        assert!(text.contains("Question"));
        assert!(text.contains("my-task"));
        assert!(text.contains("Which library?"));
        assert!(text.contains("<b>React</b> — Popular UI library"));
        assert!(text.contains("<b>Vue</b> — Progressive framework"));
    }

    #[test]
    fn format_question_text_empty_description() {
        let options = vec![("Yes".to_string(), String::new())];
        let text = format_question_text("task", "Proceed?", &options);
        assert!(text.contains("<b>Yes</b>"));
        assert!(!text.contains("—"));
    }

    #[test]
    fn format_question_text_escapes_html() {
        let options = vec![("A <b>".to_string(), "x < y & z > w".to_string())];
        let text = format_question_text("task<1>", "Use <?= tag", &options);
        assert!(text.contains("task&lt;1&gt;"));
        assert!(text.contains("Use &lt;?= tag"));
        assert!(text.contains("<b>A &lt;b&gt;</b>"));
        assert!(text.contains("x &lt; y &amp; z &gt; w"));
        // Must not contain unescaped dynamic content.
        assert!(!text.contains("task<1>"));
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

    #[test]
    fn format_perm_text_escapes_heredoc() {
        let text = format_perm_text("task", "Bash", "cat > file <<'EOF'\nsome content\nEOF");
        // The `<<` must be escaped as `&lt;&lt;`.
        assert!(text.contains("&lt;&lt;"));
        assert!(!text.contains("<<'EOF'"));
    }

    #[test]
    fn format_perm_text_truncates_long_summary() {
        let long = "x".repeat(500);
        let text = format_perm_text("task", "Bash", &long);
        // Escaped summary is still all 'x', should be truncated.
        assert!(text.contains('…'));
        // The raw 500-char string should not appear in full.
        assert!(!text.contains(&long));
    }

    #[test]
    fn html_escape_all_special_chars() {
        assert_eq!(html_escape("<>&"), "&lt;&gt;&amp;");
        assert_eq!(html_escape("\"quoted\""), "&quot;quoted&quot;");
        assert_eq!(html_escape("no specials"), "no specials");
        assert_eq!(html_escape(""), "");
    }

    #[test]
    fn truncate_within_limit() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn truncate_exceeds_limit() {
        let result = truncate("hello world", 5);
        assert_eq!(result, "hello…");
    }
}
