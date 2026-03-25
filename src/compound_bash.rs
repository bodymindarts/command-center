//! Compound Bash command approval via shfmt AST parsing.
//!
//! When Claude Code encounters a Bash command like `cargo fmt && git add -A`,
//! its `Bash(prefix:*)` permission matching rejects the whole command even
//! when every sub-command would individually be allowed. This module uses
//! `shfmt` to parse compound commands into an AST and checks each sub-command
//! against the allowed Bash patterns from `settings.local.json`.

use std::path::Path;
use std::process::Command;

use serde_json::Value;

/// Check whether a Bash command is a compound command that can be auto-approved.
///
/// Returns `true` if all sub-commands match allowed Bash patterns for the given
/// worktree. Returns `false` if any sub-command is unknown, the command is not
/// compound, or parsing fails (conservative fallthrough).
pub fn should_approve(command: &str, worktree_path: &Path) -> bool {
    // Only process compound commands
    if !is_compound(command) {
        return false;
    }

    // Parse with shfmt
    let ast = match parse_with_shfmt(command) {
        Some(ast) => ast,
        None => return false,
    };

    // Extract sub-commands from AST
    let commands = match extract_commands(&ast) {
        Some(cmds) if !cmds.is_empty() => cmds,
        _ => return false,
    };

    // Load allowed patterns from settings
    let patterns = match load_bash_patterns(worktree_path) {
        Some(p) if !p.is_empty() => p,
        _ => return false,
    };

    // Check every sub-command against allowed patterns or special-cased builtins
    commands
        .iter()
        .all(|cmd| matches_any_pattern(cmd, &patterns) || is_cd_inside_worktree(cmd, worktree_path))
}

/// Quick check for shell operators that make a command compound.
fn is_compound(command: &str) -> bool {
    command.contains("&&")
        || command.contains("||")
        || command.contains('|')
        || command.contains(';')
}

/// Shell out to `shfmt -tojson` to parse a command into a JSON AST.
fn parse_with_shfmt(command: &str) -> Option<Value> {
    let output = Command::new("shfmt")
        .args(["-tojson"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(ref mut stdin) = child.stdin {
                let _ = stdin.write_all(command.as_bytes());
                let _ = stdin.write_all(b"\n");
            }
            child.wait_with_output().ok()
        })?;

    if !output.status.success() {
        return None;
    }

    serde_json::from_slice(&output.stdout).ok()
}

/// Extract all individual commands from a shfmt JSON AST.
///
/// Returns `None` if any node type is unsupported (subshells, loops, etc.),
/// ensuring conservative fallthrough.
fn extract_commands(ast: &Value) -> Option<Vec<String>> {
    let file_type = ast.get("Type")?.as_str()?;
    if file_type != "File" {
        return None;
    }

    let stmts = ast.get("Stmts")?.as_array()?;
    let mut commands = Vec::new();
    for stmt in stmts {
        extract_from_cmd(stmt.get("Cmd")?, &mut commands)?;
    }
    Some(commands)
}

/// Recursively extract commands from a Cmd node.
///
/// Returns `None` for unsupported node types.
fn extract_from_cmd(cmd: &Value, out: &mut Vec<String>) -> Option<()> {
    let cmd_type = cmd.get("Type")?.as_str()?;
    match cmd_type {
        "BinaryCmd" => {
            // Recurse into both branches (&&, ||, |)
            extract_from_cmd(cmd.get("X")?.get("Cmd")?, out)?;
            extract_from_cmd(cmd.get("Y")?.get("Cmd")?, out)?;
        }
        "CallExpr" => {
            let args = cmd.get("Args")?.as_array()?;
            if args.is_empty() {
                return None; // No command name (bare assignment)
            }

            // Reconstruct leading literal arguments for prefix matching.
            // Stop at the first arg with non-Lit parts (quoted strings,
            // variable expansions, etc.) — this is safe because we only
            // need enough for prefix matching.
            let mut words = Vec::new();
            for arg in args {
                let parts = arg.get("Parts")?.as_array()?;
                if parts
                    .iter()
                    .all(|p| p.get("Type").and_then(|t| t.as_str()) == Some("Lit"))
                {
                    let word: String = parts
                        .iter()
                        .filter_map(|p| p.get("Value").and_then(|v| v.as_str()))
                        .collect();
                    words.push(word);
                } else {
                    break; // Stop at first non-literal arg
                }
            }
            if words.is_empty() {
                return None; // Command name is not a literal
            }
            out.push(words.join(" "));
        }
        _ => {
            // Unsupported: Subshell, Block, ForClause, IfClause, etc.
            return None;
        }
    }
    Some(())
}

/// A parsed Bash permission pattern from settings.
enum BashPattern {
    /// `Bash(prefix:*)` — command must start with prefix
    Prefix(String),
    /// `Bash(exact)` — command must exactly equal the value
    Exact(String),
}

/// Load Bash permission patterns from `settings.local.json` in the worktree.
fn load_bash_patterns(worktree_path: &Path) -> Option<Vec<BashPattern>> {
    let settings_path = worktree_path.join(".claude/settings.local.json");
    let content = std::fs::read_to_string(settings_path).ok()?;
    let settings: Value = serde_json::from_str(&content).ok()?;

    let allow = settings.get("permissions")?.get("allow")?.as_array()?;

    let patterns = allow
        .iter()
        .filter_map(|v| {
            let s = v.as_str()?;
            if !s.starts_with("Bash(") {
                return None;
            }
            let inner = s.strip_prefix("Bash(")?;
            if let Some(prefix) = inner.strip_suffix(":*)") {
                Some(BashPattern::Prefix(prefix.to_string()))
            } else {
                inner
                    .strip_suffix(')')
                    .map(|exact| BashPattern::Exact(exact.to_string()))
            }
        })
        .collect();

    Some(patterns)
}

/// Allow `cd <dir>` only when the target resolves inside the worktree.
///
/// This prevents agents from using `cd /outside && git status` to escape
/// their worktree while keeping `cd <subdir> && cargo test` working.
fn is_cd_inside_worktree(command: &str, worktree_path: &Path) -> bool {
    let rest = match command.strip_prefix("cd ") {
        Some(r) => r.trim(),
        None => return false,
    };
    if rest.is_empty() {
        return false; // bare `cd` goes to $HOME
    }
    let target = if Path::new(rest).is_absolute() {
        Path::new(rest).to_path_buf()
    } else {
        worktree_path.join(rest)
    };
    !crate::permission::is_outside_worktree(
        worktree_path.to_str().unwrap_or(""),
        target.to_str().unwrap_or(""),
    )
}

/// Check if a command string matches any allowed Bash pattern.
fn matches_any_pattern(command: &str, patterns: &[BashPattern]) -> bool {
    patterns.iter().any(|pattern| match pattern {
        BashPattern::Prefix(prefix) => {
            command == prefix || command.starts_with(&format!("{prefix} "))
        }
        BashPattern::Exact(exact) => command == exact,
    })
}

/// Check if a tool name is in the worktree's allowed permissions list.
///
/// Works for non-Bash tools (e.g. MCP tools like `mcp__style-agent__search_code`).
pub fn is_tool_allowed(tool_name: &str, worktree_path: &Path) -> bool {
    let settings_path = worktree_path.join(".claude/settings.local.json");
    let content = match std::fs::read_to_string(settings_path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let settings: Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let Some(allow) = settings
        .get("permissions")
        .and_then(|v| v.get("allow"))
        .and_then(|v| v.as_array())
    else {
        return false;
    };
    allow
        .iter()
        .any(|v| v.as_str().is_some_and(|s| s == tool_name))
}

/// Check if a simple (non-compound) command matches any allowed Bash pattern
/// in the worktree's settings.
pub fn matches_allowed_pattern(command: &str, worktree_path: &Path) -> bool {
    let patterns = match load_bash_patterns(worktree_path) {
        Some(p) if !p.is_empty() => p,
        _ => return false,
    };
    matches_any_pattern(command, &patterns)
}

/// Build the JSON response for a PreToolUse hook decision.
pub fn make_pretool_allow_response(reason: &str) -> String {
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "permissionDecisionReason": reason
        }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_compound_detects_operators() {
        assert!(is_compound("cargo fmt && git add -A"));
        assert!(is_compound("cargo fmt || echo fail"));
        assert!(is_compound("cargo test | tail -20"));
        assert!(is_compound("cargo fmt; git add"));
        assert!(!is_compound("cargo fmt --check"));
        assert!(!is_compound("git add -A"));
    }

    #[test]
    fn matches_prefix_pattern() {
        let patterns = vec![
            BashPattern::Prefix("cargo fmt".to_string()),
            BashPattern::Prefix("git add".to_string()),
        ];
        assert!(matches_any_pattern("cargo fmt", &patterns));
        assert!(matches_any_pattern("cargo fmt --check", &patterns));
        assert!(matches_any_pattern("git add -A", &patterns));
        assert!(!matches_any_pattern("cargo clippy", &patterns));
        assert!(!matches_any_pattern("rm -rf /", &patterns));
    }

    #[test]
    fn matches_exact_pattern() {
        let patterns = vec![BashPattern::Exact("pwd".to_string())];
        assert!(matches_any_pattern("pwd", &patterns));
        assert!(!matches_any_pattern("pwd -L", &patterns));
    }

    #[test]
    fn prefix_does_not_match_partial_word() {
        let patterns = vec![BashPattern::Prefix("cargo fmt".to_string())];
        // "cargo fmtx" should NOT match because it's not "cargo fmt" + space
        assert!(!matches_any_pattern("cargo fmtx", &patterns));
    }

    #[test]
    fn extract_from_simple_ast() {
        // Simulate a simple CallExpr AST node
        let ast = serde_json::json!({
            "Type": "File",
            "Stmts": [{
                "Cmd": {
                    "Type": "CallExpr",
                    "Args": [
                        {"Parts": [{"Type": "Lit", "Value": "cargo"}]},
                        {"Parts": [{"Type": "Lit", "Value": "fmt"}]}
                    ]
                }
            }]
        });
        let commands = extract_commands(&ast).unwrap();
        assert_eq!(commands, vec!["cargo fmt"]);
    }

    #[test]
    fn extract_from_binary_ast() {
        // Simulate: cargo fmt && git add -A
        let ast = serde_json::json!({
            "Type": "File",
            "Stmts": [{
                "Cmd": {
                    "Type": "BinaryCmd",
                    "X": {
                        "Cmd": {
                            "Type": "CallExpr",
                            "Args": [
                                {"Parts": [{"Type": "Lit", "Value": "cargo"}]},
                                {"Parts": [{"Type": "Lit", "Value": "fmt"}]}
                            ]
                        }
                    },
                    "Y": {
                        "Cmd": {
                            "Type": "CallExpr",
                            "Args": [
                                {"Parts": [{"Type": "Lit", "Value": "git"}]},
                                {"Parts": [{"Type": "Lit", "Value": "add"}]},
                                {"Parts": [{"Type": "Lit", "Value": "-A"}]}
                            ]
                        }
                    }
                }
            }]
        });
        let commands = extract_commands(&ast).unwrap();
        assert_eq!(commands, vec!["cargo fmt", "git add -A"]);
    }

    #[test]
    fn extract_stops_at_non_literal_args() {
        // Simulate: nix develop -c sh -c "cargo fmt"
        // The "cargo fmt" arg is DblQuoted, not Lit
        let ast = serde_json::json!({
            "Type": "File",
            "Stmts": [{
                "Cmd": {
                    "Type": "CallExpr",
                    "Args": [
                        {"Parts": [{"Type": "Lit", "Value": "nix"}]},
                        {"Parts": [{"Type": "Lit", "Value": "develop"}]},
                        {"Parts": [{"Type": "Lit", "Value": "-c"}]},
                        {"Parts": [{"Type": "Lit", "Value": "sh"}]},
                        {"Parts": [{"Type": "Lit", "Value": "-c"}]},
                        {"Parts": [{"Type": "DblQuoted", "Parts": [
                            {"Type": "Lit", "Value": "cargo fmt"}
                        ]}]}
                    ]
                }
            }]
        });
        let commands = extract_commands(&ast).unwrap();
        // Should extract up to the quoted arg
        assert_eq!(commands, vec!["nix develop -c sh -c"]);
    }

    #[test]
    fn extract_rejects_subshell() {
        let ast = serde_json::json!({
            "Type": "File",
            "Stmts": [{
                "Cmd": {
                    "Type": "Subshell",
                    "Stmts": []
                }
            }]
        });
        assert!(extract_commands(&ast).is_none());
    }

    #[test]
    fn extract_rejects_bare_assignment() {
        let ast = serde_json::json!({
            "Type": "File",
            "Stmts": [{
                "Cmd": {
                    "Type": "CallExpr",
                    "Args": []
                }
            }]
        });
        assert!(extract_commands(&ast).is_none());
    }

    #[test]
    fn extract_multiple_stmts_semicolons() {
        // Semicolons produce separate stmts
        let ast = serde_json::json!({
            "Type": "File",
            "Stmts": [
                {
                    "Cmd": {
                        "Type": "CallExpr",
                        "Args": [
                            {"Parts": [{"Type": "Lit", "Value": "cargo"}]},
                            {"Parts": [{"Type": "Lit", "Value": "fmt"}]}
                        ]
                    }
                },
                {
                    "Cmd": {
                        "Type": "CallExpr",
                        "Args": [
                            {"Parts": [{"Type": "Lit", "Value": "git"}]},
                            {"Parts": [{"Type": "Lit", "Value": "add"}]},
                            {"Parts": [{"Type": "Lit", "Value": "-A"}]}
                        ]
                    }
                }
            ]
        });
        let commands = extract_commands(&ast).unwrap();
        assert_eq!(commands, vec!["cargo fmt", "git add -A"]);
    }

    #[test]
    fn load_patterns_from_settings() {
        let dir = tempfile::tempdir().unwrap();
        let claude_dir = dir.path().join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(
            claude_dir.join("settings.local.json"),
            r#"{
                "permissions": {
                    "allow": [
                        "Read",
                        "Bash(cargo fmt:*)",
                        "Bash(git add:*)",
                        "Bash(pwd)"
                    ]
                }
            }"#,
        )
        .unwrap();

        let patterns = load_bash_patterns(dir.path()).unwrap();
        assert_eq!(patterns.len(), 3); // Read is filtered out

        assert!(matches_any_pattern("cargo fmt --check", &patterns));
        assert!(matches_any_pattern("git add -A", &patterns));
        assert!(matches_any_pattern("pwd", &patterns));
        assert!(!matches_any_pattern("rm -rf /", &patterns));
    }

    /// Integration test: requires `shfmt` on PATH.
    /// Skipped if shfmt is not available.
    #[test]
    fn integration_parse_and_extract() {
        if Command::new("shfmt").arg("--version").output().is_err() {
            eprintln!("skipping: shfmt not on PATH");
            return;
        }

        let ast = parse_with_shfmt("cargo fmt && git add -A && nix flake check").unwrap();
        let commands = extract_commands(&ast).unwrap();
        assert_eq!(commands, vec!["cargo fmt", "git add -A", "nix flake check"]);
    }

    /// Integration test: pipe command.
    #[test]
    fn integration_pipe() {
        if Command::new("shfmt").arg("--version").output().is_err() {
            eprintln!("skipping: shfmt not on PATH");
            return;
        }

        let ast = parse_with_shfmt("cargo test 2>&1 | tail -20").unwrap();
        let commands = extract_commands(&ast).unwrap();
        assert_eq!(commands, vec!["cargo test", "tail -20"]);
    }

    /// Integration test: subshell returns None.
    #[test]
    fn integration_subshell_rejected() {
        if Command::new("shfmt").arg("--version").output().is_err() {
            eprintln!("skipping: shfmt not on PATH");
            return;
        }

        let ast = parse_with_shfmt("(cargo fmt && git add -A)").unwrap();
        assert!(extract_commands(&ast).is_none());
    }

    /// Integration test: full should_approve with temp settings.
    #[test]
    fn integration_should_approve() {
        if Command::new("shfmt").arg("--version").output().is_err() {
            eprintln!("skipping: shfmt not on PATH");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let claude_dir = dir.path().join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(
            claude_dir.join("settings.local.json"),
            r#"{
                "permissions": {
                    "allow": [
                        "Bash(cargo fmt:*)",
                        "Bash(git add:*)",
                        "Bash(nix flake check:*)"
                    ]
                }
            }"#,
        )
        .unwrap();

        assert!(should_approve(
            "cargo fmt && git add -A && nix flake check",
            dir.path()
        ));
        assert!(!should_approve("cargo fmt && rm -rf /", dir.path()));
        assert!(!should_approve("cargo fmt", dir.path())); // not compound
        assert!(!should_approve("(cargo fmt && git add -A)", dir.path())); // subshell
    }

    #[test]
    fn cd_inside_worktree_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("src");
        std::fs::create_dir_all(&sub).unwrap();

        assert!(is_cd_inside_worktree(
            &format!("cd {}", sub.display()),
            dir.path()
        ));
        // Relative path
        assert!(is_cd_inside_worktree("cd src", dir.path()));
    }

    #[test]
    fn cd_outside_worktree_rejected() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_cd_inside_worktree("cd /tmp", dir.path()));
        assert!(!is_cd_inside_worktree("cd /etc", dir.path()));
    }

    #[test]
    fn bare_cd_rejected() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_cd_inside_worktree("cd", dir.path()));
        assert!(!is_cd_inside_worktree("cd ", dir.path()));
    }

    #[test]
    fn not_cd_rejected() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_cd_inside_worktree("git status", dir.path()));
    }

    /// Integration test: cd inside worktree + git status compound.
    #[test]
    fn integration_cd_worktree_compound() {
        if Command::new("shfmt").arg("--version").output().is_err() {
            eprintln!("skipping: shfmt not on PATH");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let claude_dir = dir.path().join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(
            claude_dir.join("settings.local.json"),
            r#"{
                "permissions": {
                    "allow": [
                        "Bash(git status:*)"
                    ]
                }
            }"#,
        )
        .unwrap();

        let cmd = format!("cd {} && git status", dir.path().display());
        assert!(should_approve(&cmd, dir.path()));

        // cd outside worktree should fail
        assert!(!should_approve("cd /tmp && git status", dir.path()));
    }
}
