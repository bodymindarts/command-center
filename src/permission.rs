use std::path::{Path, PathBuf};

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

pub fn scan_permission_requests(dir: &Path) -> Option<PermissionRequest> {
    list_permission_requests(dir).into_iter().next()
}

pub fn write_permission_response(dir: &Path, req_id: &str, allow: bool) {
    let resp_path = dir.join(format!("{req_id}.resp"));

    let behavior = if allow { "allow" } else { "deny" };
    let json = serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PermissionRequest",
            "decision": {
                "behavior": behavior
            }
        }
    });

    let _ = std::fs::write(resp_path, json.to_string());
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
}
