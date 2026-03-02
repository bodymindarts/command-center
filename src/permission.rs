use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::Value;

pub struct PermissionRequest {
    pub tool_name: String,
    pub tool_input_summary: String,
    pub cwd: String,
    pub permission_suggestions: Vec<Value>,
}

/// Environment variable that spawned agents read to locate the permission socket.
pub const SOCKET_ENV: &str = "CC_PERM_SOCKET";

/// Breadcrumb file written by the dashboard so that CLI-spawned tasks
/// (which don't inherit the TUI's env) can discover the active socket.
const SOCKET_BREADCRUMB: &str = ".claude/perm-socket";

/// Write the active socket path to a breadcrumb file in the project root.
pub fn write_socket_breadcrumb(project_root: &std::path::Path, sock: &std::path::Path) {
    let path = project_root.join(SOCKET_BREADCRUMB);
    let _ = std::fs::write(&path, sock.display().to_string());
}

/// Remove the breadcrumb file on shutdown.
pub fn remove_socket_breadcrumb(project_root: &std::path::Path) {
    let _ = std::fs::remove_file(project_root.join(SOCKET_BREADCRUMB));
}

/// Read the socket path from the breadcrumb file.
pub fn read_socket_breadcrumb(project_root: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(project_root.join(SOCKET_BREADCRUMB)).ok()
}

/// Generate a unique socket path for this dashboard session (includes PID).
pub fn session_socket_path() -> PathBuf {
    let tmpdir = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
    let base = std::fs::canonicalize(&tmpdir).unwrap_or_else(|_| PathBuf::from(&tmpdir));
    base.join(format!("cc-permissions-{}.sock", std::process::id()))
}

pub fn make_response_json(
    allow: bool,
    message: Option<&str>,
    updated_permissions: Option<&[Value]>,
) -> String {
    let behavior = if allow { "allow" } else { "deny" };
    let mut decision = serde_json::json!({ "behavior": behavior });
    if let Some(msg) = message {
        decision["message"] = Value::String(msg.to_string());
    }
    if allow
        && let Some(perms) = updated_permissions
        && !perms.is_empty()
    {
        decision["updatedPermissions"] = Value::Array(perms.to_vec());
    }
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PermissionRequest",
            "decision": decision
        }
    })
    .to_string()
}

/// Check if a message is a "resolved" notification from a PostToolUse hook.
/// Returns the CWD if so.
pub fn parse_resolved_json(json: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(json).ok()?;
    if parsed.get("_resolved")?.as_bool()? {
        return parsed.get("cwd").and_then(|c| c.as_str()).map(String::from);
    }
    None
}

/// Check if a message is an "idle" notification from a Stop hook.
/// Returns the CWD if so.
pub fn parse_idle_json(json: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(json).ok()?;
    if parsed.get("_idle")?.as_bool()? {
        return parsed.get("cwd").and_then(|c| c.as_str()).map(String::from);
    }
    None
}

pub fn parse_request_json(json: &str) -> Option<PermissionRequest> {
    let parsed: Value = serde_json::from_str(json).ok()?;

    // Claude Code sends tool_name and tool_input at top level.
    // Require tool_name to be present — messages without it are not permission requests.
    let tool_name = parsed
        .get("tool_name")
        .and_then(|n| n.as_str())?
        .to_string();

    let tool_input = parsed.get("tool_input");
    let tool_input_summary = summarize_tool_input(&tool_name, tool_input);

    let cwd = parsed
        .get("cwd")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();

    let permission_suggestions = parsed
        .get("permission_suggestions")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    Some(PermissionRequest {
        tool_name,
        tool_input_summary,
        cwd,
        permission_suggestions,
    })
}

pub fn gate_request() -> Result<()> {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .context("failed to read request from stdin")?;

    let sock = std::env::var(SOCKET_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|_| session_socket_path());
    match UnixStream::connect(&sock) {
        Ok(mut stream) => {
            stream
                .write_all(input.as_bytes())
                .context("failed to write to socket")?;
            stream
                .shutdown(std::net::Shutdown::Write)
                .context("failed to shutdown write")?;
            let mut response = String::new();
            stream
                .read_to_string(&mut response)
                .context("failed to read response from socket")?;
            print!("{response}");
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
            let _ = std::fs::remove_file(&sock);
            popup_fallback(&input)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => popup_fallback(&input),
        Err(e) => Err(e).context("failed to connect to permission socket"),
    }
}

fn popup_fallback(request_json: &str) -> Result<()> {
    let Some(req) = parse_request_json(request_json) else {
        print!(
            "{}",
            make_response_json(false, Some("Invalid request JSON"), None)
        );
        return Ok(());
    };

    // Check if tmux is available
    let in_tmux = std::env::var("TMUX").is_ok();
    if !in_tmux {
        print!(
            "{}",
            make_response_json(false, Some("No approval UI available"), None)
        );
        return Ok(());
    }

    let exe = std::env::current_exe().context("failed to resolve current executable")?;
    let resp_file = std::env::temp_dir().join(format!("cc-perm-{}.resp", std::process::id()));

    let popup_cmd = format!(
        "{} agent permission-prompt --tool {} --input {} --response-file {}",
        shell_escape::unix::escape(exe.display().to_string().into()),
        shell_escape::unix::escape(req.tool_name.clone().into()),
        shell_escape::unix::escape(req.tool_input_summary.clone().into()),
        shell_escape::unix::escape(resp_file.display().to_string().into()),
    );

    let status = std::process::Command::new("tmux")
        .args(["display-popup", "-E", "-w", "70", "-h", "8", &popup_cmd])
        .status()
        .context("failed to run tmux display-popup")?;

    if status.success()
        && let Ok(response) = std::fs::read_to_string(&resp_file)
    {
        print!("{response}");
        let _ = std::fs::remove_file(&resp_file);
        return Ok(());
    }

    // Popup dismissed or failed — deny
    let _ = std::fs::remove_file(&resp_file);
    print!(
        "{}",
        make_response_json(false, Some("Popup dismissed or unavailable"), None)
    );
    Ok(())
}

pub fn prompt_request(tool: &str, input_summary: &str, response_file: &str) -> Result<()> {
    use crossterm::event::{self, Event, KeyCode, KeyEventKind};

    crossterm::terminal::enable_raw_mode().context("failed to enable raw mode")?;

    // Print prompt info
    let mut stdout = std::io::stdout();
    write!(stdout, "\r\n  Tool:  {tool}\r\n")?;
    write!(stdout, "  Input: {input_summary}\r\n\r\n")?;
    write!(stdout, "  [y] approve   [n] deny\r\n")?;
    stdout.flush()?;

    let allow = loop {
        if event::poll(std::time::Duration::from_secs(300))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => break true,
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => break false,
                    _ => {}
                }
            }
        } else {
            break false; // timeout
        }
    };

    crossterm::terminal::disable_raw_mode().context("failed to disable raw mode")?;

    let response = make_response_json(allow, None, None);
    std::fs::write(response_file, &response)
        .with_context(|| format!("failed to write response to {response_file}"))?;

    Ok(())
}

pub fn start_socket_listener() -> Result<(UnixListener, PathBuf)> {
    let sock = session_socket_path();
    let _ = std::fs::remove_file(&sock);
    let listener =
        UnixListener::bind(&sock).with_context(|| format!("failed to bind {}", sock.display()))?;
    listener
        .set_nonblocking(true)
        .context("failed to set socket non-blocking")?;
    Ok((listener, sock))
}

pub(crate) fn summarize_tool_input(tool_name: &str, input: Option<&Value>) -> String {
    let Some(input) = input else {
        return String::new();
    };
    match tool_name {
        "Bash" => input
            .get("command")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string(),
        "Edit" | "Write" | "Read" => input
            .get("file_path")
            .and_then(|p| p.as_str())
            .unwrap_or("")
            .to_string(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn summarize_bash_command() {
        let input = serde_json::json!({"command": "ls -la"});
        assert_eq!(summarize_tool_input("Bash", Some(&input)), "ls -la");
    }

    #[test]
    fn summarize_file_tools() {
        let input = serde_json::json!({"file_path": "/tmp/foo.rs"});
        assert_eq!(summarize_tool_input("Edit", Some(&input)), "/tmp/foo.rs");
        assert_eq!(summarize_tool_input("Write", Some(&input)), "/tmp/foo.rs");
        assert_eq!(summarize_tool_input("Read", Some(&input)), "/tmp/foo.rs");
    }

    #[test]
    fn summarize_unknown_tool() {
        let input = serde_json::json!({"something": "else"});
        assert_eq!(summarize_tool_input("Agent", Some(&input)), "");
    }

    #[test]
    fn summarize_none_input() {
        assert_eq!(summarize_tool_input("Bash", None), "");
    }

    #[test]
    fn make_response_json_allow() {
        let json = make_response_json(true, None, None);
        let parsed: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            parsed["hookSpecificOutput"]["decision"]["behavior"]
                .as_str()
                .unwrap(),
            "allow"
        );
        assert!(parsed["hookSpecificOutput"]["decision"]["message"].is_null());
        assert!(parsed["hookSpecificOutput"]["decision"]["updatedPermissions"].is_null());
    }

    #[test]
    fn make_response_json_allow_with_updated_permissions() {
        let perms = vec![serde_json::json!({"type": "toolAlwaysAllow", "tool": "Bash"})];
        let json = make_response_json(true, None, Some(&perms));
        let parsed: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            parsed["hookSpecificOutput"]["decision"]["behavior"]
                .as_str()
                .unwrap(),
            "allow"
        );
        let updated = parsed["hookSpecificOutput"]["decision"]["updatedPermissions"]
            .as_array()
            .unwrap();
        assert_eq!(updated.len(), 1);
        assert_eq!(updated[0]["type"].as_str().unwrap(), "toolAlwaysAllow");
        assert_eq!(updated[0]["tool"].as_str().unwrap(), "Bash");
    }

    #[test]
    fn make_response_json_deny_ignores_permissions() {
        let perms = vec![serde_json::json!({"type": "toolAlwaysAllow", "tool": "Bash"})];
        let json = make_response_json(false, None, Some(&perms));
        let parsed: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            parsed["hookSpecificOutput"]["decision"]["behavior"]
                .as_str()
                .unwrap(),
            "deny"
        );
        assert!(parsed["hookSpecificOutput"]["decision"]["updatedPermissions"].is_null());
    }

    #[test]
    fn make_response_json_deny_with_message() {
        let json = make_response_json(false, Some("Timed out"), None);
        let parsed: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            parsed["hookSpecificOutput"]["decision"]["behavior"]
                .as_str()
                .unwrap(),
            "deny"
        );
        assert_eq!(
            parsed["hookSpecificOutput"]["decision"]["message"]
                .as_str()
                .unwrap(),
            "Timed out"
        );
    }

    #[test]
    fn parse_request_json_valid() {
        let json = r#"{"tool_name":"Bash","tool_input":{"command":"ls -la"},"cwd":"/home/user"}"#;
        let req = parse_request_json(json).unwrap();
        assert_eq!(req.tool_name, "Bash");
        assert_eq!(req.tool_input_summary, "ls -la");
        assert_eq!(req.cwd, "/home/user");
        assert!(req.permission_suggestions.is_empty());
    }

    #[test]
    fn parse_request_json_with_suggestions() {
        let json = r#"{"tool_name":"Bash","tool_input":{"command":"ls"},"cwd":"/tmp","permission_suggestions":[{"type":"toolAlwaysAllow","tool":"Bash"}]}"#;
        let req = parse_request_json(json).unwrap();
        assert_eq!(req.tool_name, "Bash");
        assert_eq!(req.permission_suggestions.len(), 1);
        assert_eq!(
            req.permission_suggestions[0]["type"].as_str().unwrap(),
            "toolAlwaysAllow"
        );
    }

    #[test]
    fn parse_request_json_invalid() {
        assert!(parse_request_json("not json {{{").is_none());
    }

    #[test]
    fn socket_listener_roundtrip() {
        use std::os::unix::net::UnixStream;

        let dir = TempDir::new().unwrap();
        let sock_path = dir.path().join("test.sock");

        let listener = UnixListener::bind(&sock_path).unwrap();
        listener.set_nonblocking(false).unwrap();

        let request_json =
            r#"{"tool_name":"Bash","tool_input":{"command":"echo hi"},"cwd":"/tmp"}"#;
        let req_json = request_json.to_string();

        let handle = std::thread::spawn(move || {
            let mut stream = UnixStream::connect(&sock_path).unwrap();
            std::io::Write::write_all(&mut stream, req_json.as_bytes()).unwrap();
            stream.shutdown(std::net::Shutdown::Write).unwrap();
            let mut resp = String::new();
            std::io::Read::read_to_string(&mut stream, &mut resp).unwrap();
            resp
        });

        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut stream, &mut buf).unwrap();
        let req = parse_request_json(&buf).unwrap();
        assert_eq!(req.tool_name, "Bash");

        let response = make_response_json(true, None, None);
        std::io::Write::write_all(&mut stream, response.as_bytes()).unwrap();
        drop(stream);

        let client_resp = handle.join().unwrap();
        let parsed: Value = serde_json::from_str(&client_resp).unwrap();
        assert_eq!(
            parsed["hookSpecificOutput"]["decision"]["behavior"]
                .as_str()
                .unwrap(),
            "allow"
        );
    }
}
