use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use serde::Serialize;

#[derive(Serialize)]
pub struct PermissionLogEntry {
    pub ts: String,
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_name: Option<String>,
    pub tool: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    pub outcome: String,
}

pub fn log_permission(data_dir: &Path, entry: &PermissionLogEntry) {
    let filename = format!("permission-log-{}.jsonl", entry.role);
    let logs_dir = data_dir.join("logs");
    let _ = std::fs::create_dir_all(&logs_dir);
    let path = logs_dir.join(filename);
    if let Ok(json) = serde_json::to_string(entry)
        && let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path)
    {
        let _ = writeln!(file, "{}", json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn log_entry_serializes_with_all_fields() {
        let entry = PermissionLogEntry {
            ts: "2026-03-17T10:00:00Z".to_string(),
            role: "exo".to_string(),
            task_name: None,
            tool: "Bash".to_string(),
            command: Some("git status".to_string()),
            outcome: "approved".to_string(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["role"], "exo");
        assert_eq!(parsed["tool"], "Bash");
        assert_eq!(parsed["command"], "git status");
        assert_eq!(parsed["outcome"], "approved");
        assert!(parsed.get("task_name").is_none());
    }

    #[test]
    fn log_entry_serializes_with_task_fields() {
        let entry = PermissionLogEntry {
            ts: "2026-03-17T10:00:00Z".to_string(),
            role: "engineer".to_string(),
            task_name: Some("fix-bug".to_string()),
            tool: "Edit".to_string(),
            command: None,
            outcome: "denied".to_string(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["role"], "engineer");
        assert_eq!(parsed["task_name"], "fix-bug");
        assert!(parsed.get("command").is_none());
    }

    #[test]
    fn log_permission_writes_to_correct_file() {
        let dir = TempDir::new().unwrap();
        let entry = PermissionLogEntry {
            ts: "2026-03-17T10:00:00Z".to_string(),
            role: "exo".to_string(),
            task_name: None,
            tool: "Bash".to_string(),
            command: Some("clat list".to_string()),
            outcome: "approved".to_string(),
        };
        log_permission(dir.path(), &entry);
        let content =
            std::fs::read_to_string(dir.path().join("logs/permission-log-exo.jsonl")).unwrap();
        assert!(content.contains("\"role\":\"exo\""));
        assert!(content.ends_with('\n'));
    }

    #[test]
    fn log_permission_pm_file() {
        let dir = TempDir::new().unwrap();
        let entry = PermissionLogEntry {
            ts: "2026-03-17T10:00:00Z".to_string(),
            role: "pm".to_string(),
            task_name: None,
            tool: "Bash".to_string(),
            command: Some("clat spawn test".to_string()),
            outcome: "approved".to_string(),
        };
        log_permission(dir.path(), &entry);
        assert!(dir.path().join("logs/permission-log-pm.jsonl").exists());
    }

    #[test]
    fn log_permission_engineer_file() {
        let dir = TempDir::new().unwrap();
        let entry = PermissionLogEntry {
            ts: "2026-03-17T10:00:00Z".to_string(),
            role: "engineer".to_string(),
            task_name: Some("my-task".to_string()),
            tool: "Write".to_string(),
            command: None,
            outcome: "denied".to_string(),
        };
        log_permission(dir.path(), &entry);
        assert!(
            dir.path()
                .join("logs/permission-log-engineer.jsonl")
                .exists()
        );
    }

    #[test]
    fn log_permission_researcher_file() {
        let dir = TempDir::new().unwrap();
        let entry = PermissionLogEntry {
            ts: "2026-03-17T10:00:00Z".to_string(),
            role: "researcher".to_string(),
            task_name: Some("explore-api".to_string()),
            tool: "Bash".to_string(),
            command: Some("curl https://example.com".to_string()),
            outcome: "approved".to_string(),
        };
        log_permission(dir.path(), &entry);
        assert!(
            dir.path()
                .join("logs/permission-log-researcher.jsonl")
                .exists()
        );
    }

    #[test]
    fn log_permission_appends() {
        let dir = TempDir::new().unwrap();
        for i in 0..3 {
            let entry = PermissionLogEntry {
                ts: format!("2026-03-17T10:0{i}:00Z"),
                role: "exo".to_string(),
                task_name: None,
                tool: "Bash".to_string(),
                command: Some(format!("cmd-{i}")),
                outcome: "approved".to_string(),
            };
            log_permission(dir.path(), &entry);
        }
        let content =
            std::fs::read_to_string(dir.path().join("logs/permission-log-exo.jsonl")).unwrap();
        assert_eq!(content.lines().count(), 3);
    }
}
