use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;

pub struct PermissionRequest {
    pub req_id: String,
    pub tool_name: String,
    pub tool_input_summary: String,
    pub cwd: String,
}

pub fn permissions_dir() -> PathBuf {
    let tmpdir = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(tmpdir).join("cc-permissions")
}

pub fn socket_path() -> PathBuf {
    let tmpdir = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
    let base = std::fs::canonicalize(&tmpdir).unwrap_or_else(|_| PathBuf::from(&tmpdir));
    base.join("cc-permissions.sock")
}

pub fn make_response_json(allow: bool, message: Option<&str>) -> String {
    let behavior = if allow { "allow" } else { "deny" };
    let mut decision = serde_json::json!({ "behavior": behavior });
    if let Some(msg) = message {
        decision["message"] = Value::String(msg.to_string());
    }
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PermissionRequest",
            "decision": decision
        }
    })
    .to_string()
}

pub fn parse_request_json(json: &str) -> Option<PermissionRequest> {
    let parsed: Value = serde_json::from_str(json).ok()?;

    let tool_name = parsed
        .get("tool")
        .and_then(|t| t.get("name"))
        .and_then(|n| n.as_str())
        .unwrap_or("unknown")
        .to_string();

    let tool_input = parsed.get("tool").and_then(|t| t.get("input"));
    let tool_input_summary = summarize_tool_input(&tool_name, tool_input);

    let cwd = parsed
        .get("cwd")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();

    Some(PermissionRequest {
        req_id: String::new(),
        tool_name,
        tool_input_summary,
        cwd,
    })
}

pub fn gate_request() -> Result<()> {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .context("failed to read request from stdin")?;

    let sock = socket_path();
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
            make_response_json(false, Some("Invalid request JSON"))
        );
        return Ok(());
    };

    // Check if tmux is available
    let in_tmux = std::env::var("TMUX").is_ok();
    if !in_tmux {
        print!(
            "{}",
            make_response_json(false, Some("No approval UI available"))
        );
        return Ok(());
    }

    let exe = std::env::current_exe().context("failed to resolve current executable")?;
    let resp_file = std::env::temp_dir().join(format!("cc-perm-{}.resp", std::process::id()));

    let popup_cmd = format!(
        "{} permission prompt --tool {} --input {} --response-file {}",
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
        make_response_json(false, Some("Popup dismissed or unavailable"))
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

    let response = make_response_json(allow, None);
    std::fs::write(response_file, &response)
        .with_context(|| format!("failed to write response to {response_file}"))?;

    Ok(())
}

pub fn start_socket_listener() -> Result<UnixListener> {
    let sock = socket_path();
    let _ = std::fs::remove_file(&sock);
    let listener =
        UnixListener::bind(&sock).with_context(|| format!("failed to bind {}", sock.display()))?;
    listener
        .set_nonblocking(true)
        .context("failed to set socket non-blocking")?;
    Ok(listener)
}

fn parse_req_file(path: &Path) -> Option<PermissionRequest> {
    let req_id = path.file_stem().and_then(|s| s.to_str())?.to_string();
    let content = std::fs::read_to_string(path).ok()?;
    let parsed: Value = serde_json::from_str(&content).ok()?;

    let tool_name = parsed
        .get("tool")
        .and_then(|t| t.get("name"))
        .and_then(|n| n.as_str())
        .unwrap_or("unknown")
        .to_string();

    let tool_input = parsed.get("tool").and_then(|t| t.get("input"));
    let tool_input_summary = summarize_tool_input(&tool_name, tool_input);

    let cwd = parsed
        .get("cwd")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();

    Some(PermissionRequest {
        req_id,
        tool_name,
        tool_input_summary,
        cwd,
    })
}

pub fn list_permission_requests(dir: &Path) -> Vec<PermissionRequest> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let mut requests = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("req") {
            continue;
        }
        if let Some(req) = parse_req_file(&path) {
            requests.push(req);
        }
    }
    requests
}

#[allow(dead_code)] // Removed from TUI; will be deleted with file-based IPC
pub fn scan_permission_requests(dir: &Path) -> Option<PermissionRequest> {
    list_permission_requests(dir).into_iter().next()
}

pub fn write_permission_response(dir: &Path, req_id: &str, allow: bool) {
    let resp_path = dir.join(format!("{req_id}.resp"));
    let _ = std::fs::write(resp_path, make_response_json(allow, None));
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
    fn scan_finds_req_file() {
        let dir = TempDir::new().unwrap();
        let req = serde_json::json!({
            "tool": {
                "name": "Bash",
                "input": {"command": "echo hi"}
            },
            "cwd": "/home/user/project"
        });
        std::fs::write(dir.path().join("abc-123.req"), req.to_string()).unwrap();

        let result = scan_permission_requests(dir.path()).unwrap();
        assert_eq!(result.req_id, "abc-123");
        assert_eq!(result.tool_name, "Bash");
        assert_eq!(result.tool_input_summary, "echo hi");
        assert_eq!(result.cwd, "/home/user/project");
    }

    #[test]
    fn scan_ignores_non_req_files() {
        let dir = TempDir::new().unwrap();
        let content = serde_json::json!({"tool": {"name": "Bash"}});
        std::fs::write(dir.path().join("abc.resp"), content.to_string()).unwrap();
        std::fs::write(dir.path().join("abc.tmp"), content.to_string()).unwrap();

        assert!(scan_permission_requests(dir.path()).is_none());
    }

    #[test]
    fn scan_skips_malformed_json() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("bad.req"), "not json {{{").unwrap();

        assert!(scan_permission_requests(dir.path()).is_none());
    }

    #[test]
    fn scan_empty_dir() {
        let dir = TempDir::new().unwrap();
        assert!(scan_permission_requests(dir.path()).is_none());
    }

    #[test]
    fn scan_nonexistent_dir() {
        let dir = PathBuf::from("/tmp/cc-test-nonexistent-dir-xyz");
        assert!(scan_permission_requests(&dir).is_none());
    }

    #[test]
    fn write_response_allow() {
        let dir = TempDir::new().unwrap();
        write_permission_response(dir.path(), "test-id", true);

        let content = std::fs::read_to_string(dir.path().join("test-id.resp")).unwrap();
        let parsed: Value = serde_json::from_str(&content).unwrap();

        let behavior = parsed["hookSpecificOutput"]["decision"]["behavior"]
            .as_str()
            .unwrap();
        assert_eq!(behavior, "allow");
    }

    #[test]
    fn write_response_deny() {
        let dir = TempDir::new().unwrap();
        write_permission_response(dir.path(), "test-id", false);

        let content = std::fs::read_to_string(dir.path().join("test-id.resp")).unwrap();
        let parsed: Value = serde_json::from_str(&content).unwrap();

        let behavior = parsed["hookSpecificOutput"]["decision"]["behavior"]
            .as_str()
            .unwrap();
        assert_eq!(behavior, "deny");
    }

    #[test]
    fn full_ipc_roundtrip() {
        let dir = TempDir::new().unwrap();

        // Simulate hook writing a .req file
        let req = serde_json::json!({
            "tool": {
                "name": "Write",
                "input": {"file_path": "/src/main.rs", "content": "fn main() {}"}
            },
            "cwd": "/home/user/worktree"
        });
        std::fs::write(dir.path().join("roundtrip-id.req"), req.to_string()).unwrap();

        // Simulate TUI scanning
        let result = scan_permission_requests(dir.path()).unwrap();
        assert_eq!(result.req_id, "roundtrip-id");
        assert_eq!(result.tool_name, "Write");
        assert_eq!(result.tool_input_summary, "/src/main.rs");

        // Simulate TUI writing response
        write_permission_response(dir.path(), &result.req_id, true);

        // Verify response file exists with correct schema
        let resp_content = std::fs::read_to_string(dir.path().join("roundtrip-id.resp")).unwrap();
        let resp: Value = serde_json::from_str(&resp_content).unwrap();

        assert_eq!(
            resp["hookSpecificOutput"]["hookEventName"]
                .as_str()
                .unwrap(),
            "PermissionRequest"
        );
        assert_eq!(
            resp["hookSpecificOutput"]["decision"]["behavior"]
                .as_str()
                .unwrap(),
            "allow"
        );
    }

    #[test]
    fn list_returns_all_requests() {
        let dir = TempDir::new().unwrap();
        let req1 = serde_json::json!({
            "tool": {"name": "Bash", "input": {"command": "ls"}},
            "cwd": "/a"
        });
        let req2 = serde_json::json!({
            "tool": {"name": "Write", "input": {"file_path": "/b.rs"}},
            "cwd": "/b"
        });
        std::fs::write(dir.path().join("req-1.req"), req1.to_string()).unwrap();
        std::fs::write(dir.path().join("req-2.req"), req2.to_string()).unwrap();

        let requests = list_permission_requests(dir.path());
        assert_eq!(requests.len(), 2);
    }

    #[test]
    fn list_empty_dir() {
        let dir = TempDir::new().unwrap();
        let requests = list_permission_requests(dir.path());
        assert!(requests.is_empty());
    }

    #[test]
    fn list_nonexistent_dir() {
        let dir = PathBuf::from("/tmp/cc-test-nonexistent-list-dir");
        let requests = list_permission_requests(&dir);
        assert!(requests.is_empty());
    }

    #[test]
    fn make_response_json_allow() {
        let json = make_response_json(true, None);
        let parsed: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            parsed["hookSpecificOutput"]["decision"]["behavior"]
                .as_str()
                .unwrap(),
            "allow"
        );
        assert!(parsed["hookSpecificOutput"]["decision"]["message"].is_null());
    }

    #[test]
    fn make_response_json_deny_with_message() {
        let json = make_response_json(false, Some("Timed out"));
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
        let json = r#"{"tool":{"name":"Bash","input":{"command":"ls -la"}},"cwd":"/home/user"}"#;
        let req = parse_request_json(json).unwrap();
        assert_eq!(req.tool_name, "Bash");
        assert_eq!(req.tool_input_summary, "ls -la");
        assert_eq!(req.cwd, "/home/user");
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

        let request_json = r#"{"tool":{"name":"Bash","input":{"command":"echo hi"}},"cwd":"/tmp"}"#;
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

        let response = make_response_json(true, None);
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
