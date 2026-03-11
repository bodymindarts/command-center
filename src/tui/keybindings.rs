use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::Deserialize;
use serde::de::{self, Deserializer, SeqAccess, Visitor};
use std::fmt;
use std::path::Path;

/// A single key combination: a key code plus zero or more modifiers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyCombo {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

impl KeyCombo {
    fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self { code, modifiers }
    }

    fn matches(&self, key: &KeyEvent) -> bool {
        if key.code != self.code {
            return false;
        }
        // BackTab is inherently Shift+Tab — crossterm always sets the SHIFT
        // modifier for it, so ignore SHIFT when matching BackTab.
        if self.code == KeyCode::BackTab {
            return key.modifiers & !KeyModifiers::SHIFT == self.modifiers;
        }
        key.modifiers == self.modifiers
    }

    /// Parse a string like `"Ctrl+C"`, `"Enter"`, `"j"` into a KeyCombo.
    fn parse(s: &str) -> Result<Self, String> {
        let parts: Vec<&str> = s.split('+').collect();
        if parts.is_empty() {
            return Err("empty key string".to_string());
        }

        let key_str = parts.last().unwrap().trim();
        let mut modifiers = KeyModifiers::empty();

        for &part in &parts[..parts.len() - 1] {
            match part.trim().to_lowercase().as_str() {
                "ctrl" | "control" => modifiers |= KeyModifiers::CONTROL,
                "shift" => modifiers |= KeyModifiers::SHIFT,
                "alt" => modifiers |= KeyModifiers::ALT,
                other => return Err(format!("unknown modifier: {other}")),
            }
        }

        let code = parse_key_code(key_str)?;
        Ok(Self { code, modifiers })
    }
}

fn parse_key_code(s: &str) -> Result<KeyCode, String> {
    match s.to_lowercase().as_str() {
        "enter" | "return" => Ok(KeyCode::Enter),
        "esc" | "escape" => Ok(KeyCode::Esc),
        "tab" => Ok(KeyCode::Tab),
        "backtab" => Ok(KeyCode::BackTab),
        "backspace" => Ok(KeyCode::Backspace),
        "delete" | "del" => Ok(KeyCode::Delete),
        "up" => Ok(KeyCode::Up),
        "down" => Ok(KeyCode::Down),
        "left" => Ok(KeyCode::Left),
        "right" => Ok(KeyCode::Right),
        "home" => Ok(KeyCode::Home),
        "end" => Ok(KeyCode::End),
        "pageup" => Ok(KeyCode::PageUp),
        "pagedown" => Ok(KeyCode::PageDown),
        "space" => Ok(KeyCode::Char(' ')),
        s if s.len() == 1 => Ok(KeyCode::Char(s.chars().next().unwrap())),
        other => Err(format!("unknown key: {other}")),
    }
}

/// One or more key combos that all trigger the same action.
#[derive(Clone, Debug)]
pub struct Binding(Vec<KeyCombo>);

impl Binding {
    fn single(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self(vec![KeyCombo::new(code, modifiers)])
    }

    fn keys(combos: Vec<KeyCombo>) -> Self {
        Self(combos)
    }

    pub fn matches(&self, key: &KeyEvent) -> bool {
        self.0.iter().any(|combo| combo.matches(key))
    }

    /// Format all key combos joined with `/`, e.g. "p/Esc".
    pub fn hint_all(&self) -> String {
        self.0
            .iter()
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .join("/")
    }
}

impl fmt::Display for KeyCombo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.code == KeyCode::BackTab {
            return write!(f, "S-Tab");
        }
        if self.modifiers.contains(KeyModifiers::CONTROL) {
            write!(f, "^")?;
        }
        if self.modifiers.contains(KeyModifiers::ALT) {
            write!(f, "Alt+")?;
        }
        if self.modifiers.contains(KeyModifiers::SHIFT) {
            write!(f, "S-")?;
        }
        match self.code {
            KeyCode::Char(' ') => write!(f, "Space"),
            KeyCode::Char(c) if self.modifiers.contains(KeyModifiers::CONTROL) => {
                write!(f, "{}", c.to_ascii_uppercase())
            }
            KeyCode::Char(c) => write!(f, "{c}"),
            KeyCode::Enter => write!(f, "Enter"),
            KeyCode::Esc => write!(f, "Esc"),
            KeyCode::Tab => write!(f, "Tab"),
            KeyCode::Backspace => write!(f, "\u{232b}"),
            KeyCode::Delete => write!(f, "Del"),
            KeyCode::Up => write!(f, "Up"),
            KeyCode::Down => write!(f, "Down"),
            KeyCode::Left => write!(f, "Left"),
            KeyCode::Right => write!(f, "Right"),
            KeyCode::PageUp => write!(f, "PgUp"),
            KeyCode::PageDown => write!(f, "PgDn"),
            KeyCode::Home => write!(f, "Home"),
            KeyCode::End => write!(f, "End"),
            _ => write!(f, "?"),
        }
    }
}

impl fmt::Display for Binding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(combo) = self.0.first() {
            write!(f, "{combo}")
        } else {
            Ok(())
        }
    }
}

impl<'de> Deserialize<'de> for Binding {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct BindingVisitor;

        impl<'de> Visitor<'de> for BindingVisitor {
            type Value = Binding;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a key string or array of key strings")
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Binding, E> {
                let combo = KeyCombo::parse(v).map_err(de::Error::custom)?;
                Ok(Binding(vec![combo]))
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Binding, A::Error> {
                let mut combos = Vec::new();
                while let Some(s) = seq.next_element::<String>()? {
                    combos.push(KeyCombo::parse(&s).map_err(de::Error::custom)?);
                }
                if combos.is_empty() {
                    return Err(de::Error::custom("empty key binding array"));
                }
                Ok(Binding(combos))
            }
        }

        deserializer.deserialize_any(BindingVisitor)
    }
}

// ── Section structs ──────────────────────────────────────────────

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct GlobalBindings {
    pub quit: Binding,
    pub suspend: Binding,
    pub cycle_permissions: Binding,
    pub perm_approve: Binding,
    pub perm_trust: Binding,
    pub perm_deny: Binding,
    pub focus_exo: Binding,
    pub cycle_projects: Binding,
}

impl Default for GlobalBindings {
    fn default() -> Self {
        Self {
            quit: Binding::single(KeyCode::Char('c'), KeyModifiers::CONTROL),
            suspend: Binding::single(KeyCode::Char('z'), KeyModifiers::CONTROL),
            cycle_permissions: Binding::single(KeyCode::Char('p'), KeyModifiers::CONTROL),
            perm_approve: Binding::single(KeyCode::Char('y'), KeyModifiers::CONTROL),
            perm_trust: Binding::single(KeyCode::Char('t'), KeyModifiers::CONTROL),
            perm_deny: Binding::single(KeyCode::Char('n'), KeyModifiers::CONTROL),
            focus_exo: Binding::single(KeyCode::Char('o'), KeyModifiers::CONTROL),
            cycle_projects: Binding::single(KeyCode::Char('r'), KeyModifiers::CONTROL),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct TaskListBindings {
    pub close_detail: Binding,
    pub navigate_down: Binding,
    pub navigate_up: Binding,
    pub scroll_down: Binding,
    pub scroll_up: Binding,
    pub goto_window: Binding,
    pub open_detail: Binding,
    pub close_task: Binding,
    pub reopen_task: Binding,
    pub delete_task: Binding,
    pub search: Binding,
    pub focus_chat: Binding,
    pub show_projects: Binding,
}

impl Default for TaskListBindings {
    fn default() -> Self {
        Self {
            close_detail: Binding::single(KeyCode::Esc, KeyModifiers::empty()),
            navigate_down: Binding::keys(vec![
                KeyCombo::new(KeyCode::Char('j'), KeyModifiers::empty()),
                KeyCombo::new(KeyCode::Down, KeyModifiers::empty()),
            ]),
            navigate_up: Binding::keys(vec![
                KeyCombo::new(KeyCode::Char('k'), KeyModifiers::empty()),
                KeyCombo::new(KeyCode::Up, KeyModifiers::empty()),
            ]),
            scroll_down: Binding::keys(vec![
                KeyCombo::new(KeyCode::PageDown, KeyModifiers::empty()),
                KeyCombo::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
            ]),
            scroll_up: Binding::keys(vec![
                KeyCombo::new(KeyCode::PageUp, KeyModifiers::empty()),
                KeyCombo::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
            ]),
            goto_window: Binding::single(KeyCode::Char('g'), KeyModifiers::CONTROL),
            open_detail: Binding::single(KeyCode::Enter, KeyModifiers::empty()),
            close_task: Binding::single(KeyCode::Char('x'), KeyModifiers::empty()),
            reopen_task: Binding::single(KeyCode::Char('r'), KeyModifiers::empty()),
            delete_task: Binding::single(KeyCode::Backspace, KeyModifiers::empty()),
            search: Binding::single(KeyCode::Char('/'), KeyModifiers::empty()),
            focus_chat: Binding::keys(vec![
                KeyCombo::new(KeyCode::Tab, KeyModifiers::empty()),
                KeyCombo::new(KeyCode::Char('h'), KeyModifiers::CONTROL),
            ]),
            show_projects: Binding::single(KeyCode::Char('p'), KeyModifiers::empty()),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct TaskSearchBindings {
    pub cancel: Binding,
    pub confirm: Binding,
    pub next: Binding,
    pub prev: Binding,
}

impl Default for TaskSearchBindings {
    fn default() -> Self {
        Self {
            cancel: Binding::single(KeyCode::Esc, KeyModifiers::empty()),
            confirm: Binding::single(KeyCode::Enter, KeyModifiers::empty()),
            next: Binding::keys(vec![
                KeyCombo::new(KeyCode::Down, KeyModifiers::empty()),
                KeyCombo::new(KeyCode::Tab, KeyModifiers::empty()),
                KeyCombo::new(KeyCode::Char('n'), KeyModifiers::CONTROL),
            ]),
            prev: Binding::keys(vec![
                KeyCombo::new(KeyCode::Up, KeyModifiers::empty()),
                KeyCombo::new(KeyCode::BackTab, KeyModifiers::empty()),
                KeyCombo::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
            ]),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct ProjectListBindings {
    pub navigate_down: Binding,
    pub navigate_up: Binding,
    pub search: Binding,
    pub select: Binding,
    pub delete: Binding,
    pub back: Binding,
}

impl Default for ProjectListBindings {
    fn default() -> Self {
        Self {
            navigate_down: Binding::keys(vec![
                KeyCombo::new(KeyCode::Char('j'), KeyModifiers::empty()),
                KeyCombo::new(KeyCode::Down, KeyModifiers::empty()),
            ]),
            navigate_up: Binding::keys(vec![
                KeyCombo::new(KeyCode::Char('k'), KeyModifiers::empty()),
                KeyCombo::new(KeyCode::Up, KeyModifiers::empty()),
            ]),
            search: Binding::single(KeyCode::Char('/'), KeyModifiers::empty()),
            select: Binding::single(KeyCode::Enter, KeyModifiers::empty()),
            delete: Binding::single(KeyCode::Backspace, KeyModifiers::empty()),
            back: Binding::keys(vec![
                KeyCombo::new(KeyCode::Char('p'), KeyModifiers::empty()),
                KeyCombo::new(KeyCode::Esc, KeyModifiers::empty()),
            ]),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct TaskChatBindings {
    pub close_detail: Binding,
    pub next_task: Binding,
    pub prev_task: Binding,
    pub focus_history: Binding,
    pub close_task: Binding,
    pub focus_tasks: Binding,
    pub goto_window: Binding,
    pub send: Binding,
}

impl Default for TaskChatBindings {
    fn default() -> Self {
        Self {
            close_detail: Binding::single(KeyCode::Esc, KeyModifiers::empty()),
            next_task: Binding::single(KeyCode::Tab, KeyModifiers::empty()),
            prev_task: Binding::single(KeyCode::BackTab, KeyModifiers::empty()),
            focus_history: Binding::single(KeyCode::Char('k'), KeyModifiers::CONTROL),
            close_task: Binding::single(KeyCode::Char('x'), KeyModifiers::CONTROL),
            focus_tasks: Binding::single(KeyCode::Char('l'), KeyModifiers::CONTROL),
            goto_window: Binding::single(KeyCode::Char('g'), KeyModifiers::CONTROL),
            send: Binding::single(KeyCode::Enter, KeyModifiers::empty()),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct ChatInputBindings {
    pub cancel_streaming: Binding,
    pub open_first_task: Binding,
    pub open_last_task: Binding,
    pub focus_up: Binding,
    pub close_project: Binding,
    pub focus_task_list: Binding,
    pub send: Binding,
}

impl Default for ChatInputBindings {
    fn default() -> Self {
        Self {
            cancel_streaming: Binding::single(KeyCode::Esc, KeyModifiers::empty()),
            open_first_task: Binding::single(KeyCode::Tab, KeyModifiers::empty()),
            open_last_task: Binding::single(KeyCode::BackTab, KeyModifiers::empty()),
            focus_up: Binding::single(KeyCode::Char('k'), KeyModifiers::CONTROL),
            close_project: Binding::single(KeyCode::Char('x'), KeyModifiers::CONTROL),
            focus_task_list: Binding::single(KeyCode::Char('l'), KeyModifiers::CONTROL),
            send: Binding::single(KeyCode::Enter, KeyModifiers::empty()),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct ChatHistoryBindings {
    pub navigate_down: Binding,
    pub scroll_up: Binding,
    pub scroll_down: Binding,
    pub navigate_right: Binding,
}

impl Default for ChatHistoryBindings {
    fn default() -> Self {
        Self {
            navigate_down: Binding::single(KeyCode::Char('j'), KeyModifiers::CONTROL),
            scroll_up: Binding::single(KeyCode::Char('u'), KeyModifiers::CONTROL),
            scroll_down: Binding::single(KeyCode::Char('d'), KeyModifiers::CONTROL),
            navigate_right: Binding::single(KeyCode::Char('l'), KeyModifiers::CONTROL),
        }
    }
}

// ── Top-level ────────────────────────────────────────────────────

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Keybindings {
    pub global: GlobalBindings,
    pub task_list: TaskListBindings,
    pub task_search: TaskSearchBindings,
    pub project_list: ProjectListBindings,
    pub task_chat: TaskChatBindings,
    pub chat_input: ChatInputBindings,
    pub chat_history: ChatHistoryBindings,
}

impl Keybindings {
    /// Load keybindings from a TOML file. Falls back to defaults for any
    /// missing sections or individual bindings.
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(content) => match toml::from_str(&content) {
                Ok(kb) => kb,
                Err(e) => {
                    eprintln!("warning: failed to parse {}: {e}", path.display());
                    Self::default()
                }
            },
            Err(_) => Self::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn parse_simple_char() {
        let combo = KeyCombo::parse("j").unwrap();
        assert_eq!(combo.code, KeyCode::Char('j'));
        assert_eq!(combo.modifiers, KeyModifiers::empty());
    }

    #[test]
    fn parse_ctrl_char() {
        let combo = KeyCombo::parse("Ctrl+c").unwrap();
        assert_eq!(combo.code, KeyCode::Char('c'));
        assert_eq!(combo.modifiers, KeyModifiers::CONTROL);
    }

    #[test]
    fn parse_special_key() {
        let combo = KeyCombo::parse("Enter").unwrap();
        assert_eq!(combo.code, KeyCode::Enter);
        assert_eq!(combo.modifiers, KeyModifiers::empty());
    }

    #[test]
    fn parse_ctrl_special() {
        let combo = KeyCombo::parse("Ctrl+G").unwrap();
        assert_eq!(combo.code, KeyCode::Char('g'));
        assert_eq!(combo.modifiers, KeyModifiers::CONTROL);
    }

    #[test]
    fn parse_unknown_key_fails() {
        assert!(KeyCombo::parse("FooBar").is_err());
    }

    #[test]
    fn parse_unknown_modifier_fails() {
        assert!(KeyCombo::parse("Meta+c").is_err());
    }

    #[test]
    fn binding_matches_single() {
        let binding = Binding::single(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(binding.matches(&press(KeyCode::Char('c'), KeyModifiers::CONTROL)));
        assert!(!binding.matches(&press(KeyCode::Char('c'), KeyModifiers::empty())));
        assert!(!binding.matches(&press(KeyCode::Char('x'), KeyModifiers::CONTROL)));
    }

    #[test]
    fn binding_matches_multi() {
        let binding = Binding::keys(vec![
            KeyCombo::new(KeyCode::Char('j'), KeyModifiers::empty()),
            KeyCombo::new(KeyCode::Down, KeyModifiers::empty()),
        ]);
        assert!(binding.matches(&press(KeyCode::Char('j'), KeyModifiers::empty())));
        assert!(binding.matches(&press(KeyCode::Down, KeyModifiers::empty())));
        assert!(!binding.matches(&press(KeyCode::Char('k'), KeyModifiers::empty())));
    }

    #[test]
    fn deserialize_single_string() {
        let toml_str = r#"quit = "Ctrl+c""#;
        #[derive(Deserialize)]
        struct Test {
            quit: Binding,
        }
        let t: Test = toml::from_str(toml_str).unwrap();
        assert!(
            t.quit
                .matches(&press(KeyCode::Char('c'), KeyModifiers::CONTROL))
        );
    }

    #[test]
    fn deserialize_array() {
        let toml_str = r#"nav = ["j", "Down"]"#;
        #[derive(Deserialize)]
        struct Test {
            nav: Binding,
        }
        let t: Test = toml::from_str(toml_str).unwrap();
        assert!(
            t.nav
                .matches(&press(KeyCode::Char('j'), KeyModifiers::empty()))
        );
        assert!(t.nav.matches(&press(KeyCode::Down, KeyModifiers::empty())));
    }

    #[test]
    fn deserialize_partial_config() {
        let toml_str = r#"
[global]
quit = "q"
"#;
        let kb: Keybindings = toml::from_str(toml_str).unwrap();
        // quit should be overridden
        assert!(
            kb.global
                .quit
                .matches(&press(KeyCode::Char('q'), KeyModifiers::empty()))
        );
        // other global bindings should be defaults
        assert!(
            kb.global
                .cycle_permissions
                .matches(&press(KeyCode::Char('p'), KeyModifiers::CONTROL))
        );
        // other sections should be defaults
        assert!(
            kb.task_list
                .navigate_down
                .matches(&press(KeyCode::Char('j'), KeyModifiers::empty()))
        );
    }

    #[test]
    fn display_ctrl_char() {
        let combo = KeyCombo::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(combo.to_string(), "^C");
    }

    #[test]
    fn display_plain_char() {
        let combo = KeyCombo::new(KeyCode::Char('j'), KeyModifiers::empty());
        assert_eq!(combo.to_string(), "j");
    }

    #[test]
    fn display_special_keys() {
        assert_eq!(
            KeyCombo::new(KeyCode::Enter, KeyModifiers::empty()).to_string(),
            "Enter"
        );
        assert_eq!(
            KeyCombo::new(KeyCode::Esc, KeyModifiers::empty()).to_string(),
            "Esc"
        );
        assert_eq!(
            KeyCombo::new(KeyCode::Tab, KeyModifiers::empty()).to_string(),
            "Tab"
        );
        assert_eq!(
            KeyCombo::new(KeyCode::BackTab, KeyModifiers::empty()).to_string(),
            "S-Tab"
        );
        assert_eq!(
            KeyCombo::new(KeyCode::Backspace, KeyModifiers::empty()).to_string(),
            "\u{232b}"
        );
    }

    #[test]
    fn display_binding_shows_first_combo() {
        let binding = Binding::keys(vec![
            KeyCombo::new(KeyCode::Char('j'), KeyModifiers::empty()),
            KeyCombo::new(KeyCode::Down, KeyModifiers::empty()),
        ]);
        assert_eq!(binding.to_string(), "j");
    }

    #[test]
    fn hint_all_joins_combos() {
        let binding = Binding::keys(vec![
            KeyCombo::new(KeyCode::Char('p'), KeyModifiers::empty()),
            KeyCombo::new(KeyCode::Esc, KeyModifiers::empty()),
        ]);
        assert_eq!(binding.hint_all(), "p/Esc");
    }

    #[test]
    fn default_keybindings_match_original() {
        let kb = Keybindings::default();
        // Global
        assert!(
            kb.global
                .quit
                .matches(&press(KeyCode::Char('c'), KeyModifiers::CONTROL))
        );
        assert!(
            kb.global
                .suspend
                .matches(&press(KeyCode::Char('z'), KeyModifiers::CONTROL))
        );
        assert!(
            kb.global
                .focus_exo
                .matches(&press(KeyCode::Char('o'), KeyModifiers::CONTROL))
        );
        // Task list — multiple bindings
        assert!(
            kb.task_list
                .navigate_down
                .matches(&press(KeyCode::Char('j'), KeyModifiers::empty()))
        );
        assert!(
            kb.task_list
                .navigate_down
                .matches(&press(KeyCode::Down, KeyModifiers::empty()))
        );
        assert!(
            kb.task_list
                .scroll_down
                .matches(&press(KeyCode::PageDown, KeyModifiers::empty()))
        );
        assert!(
            kb.task_list
                .scroll_down
                .matches(&press(KeyCode::Char('d'), KeyModifiers::CONTROL))
        );
    }
}
