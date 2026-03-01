You are a software engineer.

## Your task
Fix ExO chat text not appearing in the TUI.

## Root cause (already diagnosed)
In src/tui/claude.rs, the spawn_claude function parses stream-json output events. The code matches on 'content_block_delta' events to extract text, but the Claude Code CLI's stream-json format does NOT emit content_block_delta events. Instead, it sends complete 'assistant' event types with the full message content.

The actual event structure is:
{"type":"assistant","message":{"content":[{"type":"text","text":"response here"}],...}}

The current code has a comment at line ~162 saying 'assistant' events are 'informational' and ignores them. This is wrong — the text content lives there.

## Fix
In the match on event_type in spawn_claude (around line 131):
1. Add an 'assistant' arm that extracts text from message.content array
2. For each content block where type=='text', send ExoEvent::TextDelta with the text
3. Also handle tool_use content blocks by sending ExoEvent::ToolStart with the tool name

The format for the content array is: message.content is an array of objects. Each has a 'type' field — either 'text' (with a 'text' field) or 'tool_use' (with a 'name' field).

Keep the existing content_block_delta/content_block_start matching too as a fallback in case some versions of the CLI do emit those.

## Test
Run: CLAUDECODE= claude -p 'Say hi' --output-format stream-json --verbose --input-format stream-json
(with a JSON message on stdin like: {"type":"user","message":{"role":"user","content":"Say hi"},"session_id":"test"})
And verify the assistant event is parsed correctly.

## Workflow
- Read and understand existing code before making changes
- Run `cargo fmt && git add -A && nix flake check` before committing
- Use conventional commits: `type(scope): description`
- Keep changes minimal and focused — don't over-engineer

## Constraints
- All dependencies are declared in flake.nix — never assume binaries are installed
- Clippy runs with --deny warnings — no dead code, no unused imports
- Tests run via nextest