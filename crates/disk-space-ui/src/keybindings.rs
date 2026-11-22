//! Customizable keyboard and extra-mouse-button bindings with disk persistence.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use nsh_core::keymap::Keystroke;
use nsh_core::keymap::Modifiers;
use nsh_core::platform::Os;
use nsh_core::prelude::KeyEvent;
use nsh_core::prelude::MouseButton;
use nsh_core::prelude::key_label;

/// Logical section grouping in the keyboard & mouse settings dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ActionSection {
    Navigation,
    Selection,
    FileActions,
    Workspace,
}

impl ActionSection {
    pub(crate) const ALL: [Self; 4] = [
        Self::Navigation,
        Self::Selection,
        Self::FileActions,
        Self::Workspace,
    ];

    #[must_use]
    pub(crate) const fn title(self) -> &'static str {
        match self {
            Self::Navigation => "Navigation",
            Self::Selection => "Selection & List",
            Self::FileActions => "File Actions",
            Self::Workspace => "Workspace & Queue",
        }
    }
}

/// Every user-bindable command across the application.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Action {
    NavigateBack,
    NavigateForward,
    OpenSelected,
    AscendDirectory,
    FocusSearch,
    JumpToCulprit,
    MoveDown,
    MoveUp,
    SelectAll,
    QuickLook,
    SnagSelected,
    RevealInFinder,
    ToggleJunkFilter,
    ToggleQueueDrawer,
    CycleViewTab,
    RescanRoot,
    ReviewAndExecute,
    OpenSettings,
}

impl Action {
    pub(crate) const ALL: [Self; 18] = [
        Self::NavigateBack,
        Self::NavigateForward,
        Self::OpenSelected,
        Self::AscendDirectory,
        Self::FocusSearch,
        Self::JumpToCulprit,
        Self::MoveDown,
        Self::MoveUp,
        Self::SelectAll,
        Self::QuickLook,
        Self::SnagSelected,
        Self::RevealInFinder,
        Self::ToggleJunkFilter,
        Self::ToggleQueueDrawer,
        Self::CycleViewTab,
        Self::RescanRoot,
        Self::ReviewAndExecute,
        Self::OpenSettings,
    ];

    #[must_use]
    pub(crate) const fn section(self) -> ActionSection {
        match self {
            Self::NavigateBack
            | Self::NavigateForward
            | Self::OpenSelected
            | Self::AscendDirectory
            | Self::FocusSearch
            | Self::JumpToCulprit => ActionSection::Navigation,
            Self::MoveDown | Self::MoveUp | Self::SelectAll => ActionSection::Selection,
            Self::QuickLook | Self::SnagSelected | Self::RevealInFinder => {
                ActionSection::FileActions
            }
            Self::ToggleJunkFilter
            | Self::ToggleQueueDrawer
            | Self::CycleViewTab
            | Self::RescanRoot
            | Self::ReviewAndExecute
            | Self::OpenSettings => ActionSection::Workspace,
        }
    }

    #[must_use]
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::NavigateBack => "Navigate Back",
            Self::NavigateForward => "Navigate Forward",
            Self::OpenSelected => "Open Folder / File",
            Self::AscendDirectory => "Go to Parent Folder",
            Self::FocusSearch => "Focus Search",
            Self::JumpToCulprit => "Jump to Largest Culprit",
            Self::MoveDown => "Move Down",
            Self::MoveUp => "Move Up",
            Self::SelectAll => "Select All Visible",
            Self::QuickLook => "Quick Look Preview",
            Self::SnagSelected => "Snag Selected Item(s)",
            Self::RevealInFinder => "Reveal in File Manager",
            Self::ToggleJunkFilter => "Toggle Junk & Unused Filter",
            Self::ToggleQueueDrawer => "Toggle Destruction Queue",
            Self::CycleViewTab => "Cycle Visualization Tab",
            Self::RescanRoot => "Rescan Root",
            Self::ReviewAndExecute => "Review & Execute Queue",
            Self::OpenSettings => "Open Shortcut Settings",
        }
    }

    #[must_use]
    pub(crate) const fn config_key(self) -> &'static str {
        match self {
            Self::NavigateBack => "navigate_back",
            Self::NavigateForward => "navigate_forward",
            Self::OpenSelected => "open_selected",
            Self::AscendDirectory => "ascend_directory",
            Self::FocusSearch => "focus_search",
            Self::JumpToCulprit => "jump_to_culprit",
            Self::MoveDown => "move_down",
            Self::MoveUp => "move_up",
            Self::SelectAll => "select_all",
            Self::QuickLook => "quick_look",
            Self::SnagSelected => "snag_selected",
            Self::RevealInFinder => "reveal_in_finder",
            Self::ToggleJunkFilter => "toggle_junk_filter",
            Self::ToggleQueueDrawer => "toggle_queue_drawer",
            Self::CycleViewTab => "cycle_view_tab",
            Self::RescanRoot => "rescan_root",
            Self::ReviewAndExecute => "review_and_execute",
            Self::OpenSettings => "open_settings",
        }
    }

    #[must_use]
    pub(crate) fn from_config_key(key: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|action| action.config_key() == key)
    }

    #[must_use]
    pub(crate) fn default_bindings(self) -> Vec<InputBinding> {
        match self {
            Self::NavigateBack => vec![
                InputBinding::key("mod+["),
                InputBinding::mouse(MouseButton::Back, Modifiers::default()),
            ],
            Self::NavigateForward => vec![
                InputBinding::key("mod+]"),
                InputBinding::mouse(MouseButton::Forward, Modifiers::default()),
            ],
            Self::OpenSelected => vec![InputBinding::key("enter"), InputBinding::key("right")],
            Self::AscendDirectory => vec![
                InputBinding::key("left"),
                InputBinding::key("h"),
                InputBinding::key("backspace"),
            ],
            Self::FocusSearch => vec![InputBinding::key("/"), InputBinding::key("mod+f")],
            Self::JumpToCulprit => vec![InputBinding::key("g")],
            Self::MoveDown => vec![InputBinding::key("down"), InputBinding::key("j")],
            Self::MoveUp => vec![InputBinding::key("up"), InputBinding::key("k")],
            Self::SelectAll => vec![InputBinding::key("mod+a")],
            Self::QuickLook => vec![InputBinding::key("space")],
            Self::SnagSelected => vec![InputBinding::key("s"), InputBinding::key("shift+s")],
            Self::RevealInFinder => vec![InputBinding::key("o")],
            Self::ToggleJunkFilter => vec![InputBinding::key("f")],
            Self::ToggleQueueDrawer => vec![InputBinding::key("d")],
            Self::CycleViewTab => vec![InputBinding::key("v")],
            Self::RescanRoot => vec![InputBinding::key("r")],
            Self::ReviewAndExecute => vec![InputBinding::key("mod+enter")],
            Self::OpenSettings => vec![InputBinding::key("mod+,")],
        }
    }
}

/// Either a keyboard shortcut (canonical NSH keystroke) or an extra mouse button binding.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum InputBinding {
    Key(String),
    Mouse {
        button: MouseButton,
        modifiers: Modifiers,
    },
}

impl InputBinding {
    #[must_use]
    pub(crate) fn key(spec: impl Into<String>) -> Self {
        Self::Key(spec.into())
    }

    #[must_use]
    pub(crate) const fn mouse(button: MouseButton, modifiers: Modifiers) -> Self {
        Self::Mouse { button, modifiers }
    }

    /// Formats this binding for display inside an active NSH UI context.
    #[must_use]
    pub(crate) fn display_label(&self) -> String {
        match self {
            Self::Key(spec) => {
                if is_valid_keystroke(spec) {
                    key_label(spec)
                } else {
                    spec.clone()
                }
            }
            Self::Mouse { button, modifiers } => format_mouse_label(*button, *modifiers),
        }
    }

    /// Serializes to a compact token for `keybindings.conf`.
    #[must_use]
    pub(crate) fn serialize(&self) -> String {
        match self {
            Self::Key(spec) => format!("key:{spec}"),
            Self::Mouse { button, modifiers } => {
                let mut parts = Vec::new();
                if modifiers.ctrl {
                    parts.push("ctrl");
                }
                if modifiers.alt {
                    parts.push("alt");
                }
                if modifiers.shift {
                    parts.push("shift");
                }
                if modifiers.cmd {
                    parts.push("cmd");
                }
                parts.push(mouse_button_token(*button));
                format!("mouse:{}", parts.join("+"))
            }
        }
    }

    /// Parses a serialized token from `keybindings.conf`.
    #[must_use]
    pub(crate) fn parse_serialized(token: &str) -> Option<Self> {
        let trimmed = token.trim();
        if let Some(spec) = trimmed.strip_prefix("key:") {
            let clean = spec.trim().to_ascii_lowercase();
            return is_valid_keystroke(&clean).then_some(Self::Key(clean));
        }
        if let Some(mouse_spec) = trimmed.strip_prefix("mouse:") {
            let mut modifiers = Modifiers::default();
            let mut button = None;
            for part in mouse_spec.trim().to_ascii_lowercase().split('+') {
                match part {
                    "ctrl" | "control" => modifiers.ctrl = true,
                    "alt" | "opt" | "option" => modifiers.alt = true,
                    "shift" => modifiers.shift = true,
                    "cmd" | "mod" | "meta" | "super" => modifiers.cmd = true,
                    "back" => button = Some(MouseButton::Back),
                    "forward" => button = Some(MouseButton::Forward),
                    "middle" => button = Some(MouseButton::Middle),
                    _ => return None,
                }
            }
            return button.map(|btn| Self::Mouse {
                button: btn,
                modifiers,
            });
        }
        None
    }
}

/// Complete map of active bindings for every [`Action`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeyBindings {
    map: BTreeMap<Action, Vec<InputBinding>>,
}

impl Default for KeyBindings {
    fn default() -> Self {
        let mut map = BTreeMap::new();
        for action in Action::ALL {
            map.insert(action, action.default_bindings());
        }
        Self { map }
    }
}

impl KeyBindings {
    #[must_use]
    pub(crate) fn bindings_for(&self, action: Action) -> &[InputBinding] {
        self.map.get(&action).map_or(&[], Vec::as_slice)
    }

    pub(crate) fn key_strings_for(&self, action: Action) -> impl Iterator<Item = &str> {
        self.bindings_for(action)
            .iter()
            .filter_map(|binding| match binding {
                InputBinding::Key(spec) => Some(spec.as_str()),
                InputBinding::Mouse { .. } => None,
            })
    }

    /// Short display label for the primary binding of `action` (used in inline hints/buttons).
    #[must_use]
    pub(crate) fn primary_label(&self, action: Action) -> Option<String> {
        self.bindings_for(action)
            .first()
            .map(InputBinding::display_label)
    }

    /// Finds the [`Action`] bound to a specific extra mouse button + modifiers, if any.
    #[must_use]
    pub(crate) fn action_for_mouse(
        &self,
        button: MouseButton,
        modifiers: Modifiers,
    ) -> Option<Action> {
        let target = InputBinding::Mouse { button, modifiers };
        for (&action, list) in &self.map {
            if list.iter().any(|b| b == &target) {
                return Some(action);
            }
        }
        None
    }

    /// Assigns `binding` to `action` (`replace = true` replaces all existing bindings for `action`;
    /// `replace = false` appends if not already present). Removes any conflicting binding from
    /// another action and returns the displaced action if one was modified.
    pub(crate) fn set_binding(
        &mut self,
        action: Action,
        binding: InputBinding,
        replace: bool,
    ) -> Option<Action> {
        let mut displaced = None;
        for (&other, list) in &mut self.map {
            if other != action {
                let before = list.len();
                list.retain(|b| b != &binding);
                if list.len() != before && displaced.is_none() {
                    displaced = Some(other);
                }
            }
        }

        let entry = self.map.entry(action).or_default();
        if replace {
            entry.clear();
            entry.push(binding);
        } else if !entry.contains(&binding) {
            entry.push(binding);
        }
        displaced
    }

    pub(crate) fn remove_binding(&mut self, action: Action, index: usize) {
        if let Some(list) = self.map.get_mut(&action)
            && index < list.len()
        {
            list.remove(index);
        }
    }

    pub(crate) fn reset_action(&mut self, action: Action) {
        self.map.insert(action, action.default_bindings());
    }

    pub(crate) fn reset_all(&mut self) {
        *self = Self::default();
    }

    #[must_use]
    pub(crate) fn serialize_config(&self) -> String {
        let mut out = String::from("# disk-space keybindings\n");
        for action in Action::ALL {
            let tokens: Vec<String> = self
                .bindings_for(action)
                .iter()
                .map(InputBinding::serialize)
                .collect();
            out.push_str(action.config_key());
            out.push('=');
            out.push_str(&tokens.join(","));
            out.push('\n');
        }
        out
    }

    #[must_use]
    pub(crate) fn parse_config(contents: &str) -> Self {
        let mut kb = Self::default();
        for raw_line in contents.lines() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, val)) = line.split_once('=') else {
                continue;
            };
            let Some(action) = Action::from_config_key(key.trim()) else {
                continue;
            };
            let trimmed_val = val.trim();
            if trimmed_val.is_empty() {
                kb.map.insert(action, Vec::new());
                continue;
            }
            let parsed: Vec<InputBinding> = trimmed_val
                .split(',')
                .filter_map(InputBinding::parse_serialized)
                .collect();
            if !parsed.is_empty() {
                kb.map.insert(action, parsed);
            }
        }
        kb
    }

    #[must_use]
    pub(crate) fn load_from_path(path: &Path) -> Self {
        fs::read_to_string(path).map_or_else(|_| Self::default(), |text| Self::parse_config(&text))
    }

    pub(crate) fn save_to_path(&self, path: &Path) -> Result<(), std::io::Error> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, self.serialize_config())
    }
}

/// Converts a raw [`KeyEvent`] from the Settings recorder into a validated [`InputBinding::Key`].
/// Returns `None` for modifier-only keys, auto-repeats, bare `Escape`, or OS-reserved keystrokes.
#[must_use]
pub(crate) fn key_event_to_binding(ev: &KeyEvent) -> Option<InputBinding> {
    if ev.repeat {
        return None;
    }
    let key_str = ev.key.as_str();
    if matches!(
        key_str,
        "Shift"
            | "Control"
            | "Alt"
            | "Meta"
            | "CapsLock"
            | "NumLock"
            | "ScrollLock"
            | "Fn"
            | "Unidentified"
    ) {
        return None;
    }
    if key_str.eq_ignore_ascii_case("Escape") && ev.modifiers == Modifiers::default() {
        return None;
    }

    let base_key = map_event_key_name(key_str, ev.code.as_str())?;
    let mut parts: Vec<String> = Vec::new();
    let is_apple = matches!(compile_target_os(), Os::MacOs | Os::Ios);

    if is_apple {
        if ev.modifiers.cmd {
            parts.push("mod".to_owned());
        }
        if ev.modifiers.ctrl {
            parts.push("ctrl".to_owned());
        }
    } else {
        if ev.modifiers.ctrl {
            parts.push("mod".to_owned());
        }
        if ev.modifiers.cmd {
            parts.push("cmd".to_owned());
        }
    }
    if ev.modifiers.alt {
        parts.push("alt".to_owned());
    }
    if ev.modifiers.shift {
        parts.push("shift".to_owned());
    }
    parts.push(base_key);

    let spec = parts.join("+");
    is_valid_keystroke(&spec).then_some(InputBinding::Key(spec))
}

/// Returns `true` if `spec` parses as a valid, non-OS-reserved NSH keystroke.
#[must_use]
pub(crate) fn is_valid_keystroke(spec: &str) -> bool {
    let target_os = compile_target_os();
    Keystroke::parse(spec, target_os).is_ok_and(|ks| ks.reserved(target_os).is_none())
}

const fn compile_target_os() -> Os {
    #[cfg(target_os = "macos")]
    {
        Os::MacOs
    }
    #[cfg(target_os = "windows")]
    {
        Os::Windows
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Os::Linux
    }
}

fn map_event_key_name(key: &str, code: &str) -> Option<String> {
    if let Some(named) = map_dom_code_to_key(code) {
        return Some(named.to_owned());
    }
    let named = match key {
        " " | "Spacebar" | "Space" => "space",
        "Enter" => "enter",
        "Tab" => "tab",
        "Backspace" => "backspace",
        "Delete" => "delete",
        "ArrowUp" | "Up" => "up",
        "ArrowDown" | "Down" => "down",
        "ArrowLeft" | "Left" => "left",
        "ArrowRight" | "Right" => "right",
        "Home" => "home",
        "End" => "end",
        "PageUp" => "pageup",
        "PageDown" => "pagedown",
        other => {
            let lower = other.to_ascii_lowercase();
            if let Some(num) = lower
                .strip_prefix('f')
                .and_then(|n| n.parse::<u8>().ok())
                .filter(|n| (1..=24).contains(n))
            {
                return Some(format!("f{num}"));
            }
            let mut chars = other.chars();
            let (Some(ch), None) = (chars.next(), chars.next()) else {
                return None;
            };
            let base = unshift_us_char(ch).to_ascii_lowercase();
            if base.is_ascii_graphic() {
                return Some(base.to_string());
            }
            return None;
        }
    };
    Some(named.to_owned())
}

fn map_dom_code_to_key(code: &str) -> Option<&'static str> {
    if let Some(rest) = code.strip_prefix("Key")
        && rest.len() == 1
    {
        return match rest {
            "A" => Some("a"),
            "B" => Some("b"),
            "C" => Some("c"),
            "D" => Some("d"),
            "E" => Some("e"),
            "F" => Some("f"),
            "G" => Some("g"),
            "H" => Some("h"),
            "I" => Some("i"),
            "J" => Some("j"),
            "K" => Some("k"),
            "L" => Some("l"),
            "M" => Some("m"),
            "N" => Some("n"),
            "O" => Some("o"),
            "P" => Some("p"),
            "Q" => Some("q"),
            "R" => Some("r"),
            "S" => Some("s"),
            "T" => Some("t"),
            "U" => Some("u"),
            "V" => Some("v"),
            "W" => Some("w"),
            "X" => Some("x"),
            "Y" => Some("y"),
            "Z" => Some("z"),
            _ => None,
        };
    }
    if let Some(rest) = code.strip_prefix("Digit")
        && rest.len() == 1
    {
        return match rest {
            "0" => Some("0"),
            "1" => Some("1"),
            "2" => Some("2"),
            "3" => Some("3"),
            "4" => Some("4"),
            "5" => Some("5"),
            "6" => Some("6"),
            "7" => Some("7"),
            "8" => Some("8"),
            "9" => Some("9"),
            _ => None,
        };
    }
    match code {
        "Space" => Some("space"),
        "Enter" | "NumpadEnter" => Some("enter"),
        "Tab" => Some("tab"),
        "Backspace" => Some("backspace"),
        "Delete" => Some("delete"),
        "ArrowUp" => Some("up"),
        "ArrowDown" => Some("down"),
        "ArrowLeft" => Some("left"),
        "ArrowRight" => Some("right"),
        "Home" => Some("home"),
        "End" => Some("end"),
        "PageUp" => Some("pageup"),
        "PageDown" => Some("pagedown"),
        "BracketLeft" => Some("["),
        "BracketRight" => Some("]"),
        "Slash" => Some("/"),
        "Backslash" => Some("\\"),
        "Comma" => Some(","),
        "Period" => Some("."),
        "Semicolon" => Some(";"),
        "Quote" => Some("'"),
        "Minus" => Some("-"),
        "Equal" => Some("="),
        "Backquote" => Some("`"),
        _ => None,
    }
}

const fn unshift_us_char(ch: char) -> char {
    match ch {
        '!' => '1',
        '@' => '2',
        '#' => '3',
        '$' => '4',
        '%' => '5',
        '^' => '6',
        '&' => '7',
        '*' => '8',
        '(' => '9',
        ')' => '0',
        '_' => '-',
        '+' => '=',
        '{' => '[',
        '}' => ']',
        '|' => '\\',
        ':' => ';',
        '"' => '\'',
        '<' => ',',
        '>' => '.',
        '?' => '/',
        '~' => '`',
        other => other,
    }
}

const fn mouse_button_token(button: MouseButton) -> &'static str {
    match button {
        MouseButton::Back => "back",
        MouseButton::Forward => "forward",
        MouseButton::Middle => "middle",
        MouseButton::Left => "left",
        MouseButton::Right => "right",
    }
}

fn format_mouse_label(button: MouseButton, modifiers: Modifiers) -> String {
    let mut parts = Vec::new();
    let is_apple = matches!(compile_target_os(), Os::MacOs | Os::Ios);
    if modifiers.ctrl {
        parts.push(if is_apple { "⌃" } else { "Ctrl+" });
    }
    if modifiers.alt {
        parts.push(if is_apple { "⌥" } else { "Alt+" });
    }
    if modifiers.shift {
        parts.push(if is_apple { "⇧" } else { "Shift+" });
    }
    if modifiers.cmd {
        parts.push(if is_apple { "⌘" } else { "Super+" });
    }
    let btn_label = match button {
        MouseButton::Back => "Mouse Back",
        MouseButton::Forward => "Mouse Forward",
        MouseButton::Middle => "Mouse Middle",
        MouseButton::Left => "Mouse Left",
        MouseButton::Right => "Mouse Right",
    };
    parts.push(btn_label);
    parts.concat()
}

#[cfg(test)]
mod tests {
    use nsh_core::prelude::SharedString;

    use super::*;

    #[test]
    fn default_keybindings_round_trip_through_config_serialization() {
        let mut kb = KeyBindings::default();
        assert_eq!(
            kb.action_for_mouse(MouseButton::Back, Modifiers::default()),
            Some(Action::NavigateBack)
        );
        assert_eq!(
            kb.action_for_mouse(MouseButton::Forward, Modifiers::default()),
            Some(Action::NavigateForward)
        );

        let displaced = kb.set_binding(
            Action::QuickLook,
            InputBinding::mouse(MouseButton::Middle, Modifiers::default()),
            false,
        );
        assert_eq!(displaced, None);

        let displaced_back = kb.set_binding(
            Action::SnagSelected,
            InputBinding::mouse(MouseButton::Back, Modifiers::default()),
            false,
        );
        assert_eq!(displaced_back, Some(Action::NavigateBack));
        assert_eq!(
            kb.action_for_mouse(MouseButton::Back, Modifiers::default()),
            Some(Action::SnagSelected)
        );

        let serialized = kb.serialize_config();
        let reloaded = KeyBindings::parse_config(&serialized);
        assert_eq!(reloaded, kb);
    }

    #[test]
    fn key_event_conversion_handles_space_shifted_keys_and_rejects_reserved() {
        let mut space_ev = KeyEvent::default();
        space_ev.key = SharedString::new_static(" ");
        space_ev.code = SharedString::new_static("Space");
        assert_eq!(
            key_event_to_binding(&space_ev),
            Some(InputBinding::key("space"))
        );

        let mut mod_bracket = KeyEvent::default();
        mod_bracket.key = SharedString::new_static("[");
        mod_bracket.code = SharedString::new_static("BracketLeft");
        mod_bracket.modifiers.cmd = true;
        #[cfg(target_os = "macos")]
        assert_eq!(
            key_event_to_binding(&mod_bracket),
            Some(InputBinding::key("mod+["))
        );

        let mut esc = KeyEvent::default();
        esc.key = SharedString::new_static("Escape");
        assert_eq!(key_event_to_binding(&esc), None);

        let mut shift_only = KeyEvent::default();
        shift_only.key = SharedString::new_static("Shift");
        assert_eq!(key_event_to_binding(&shift_only), None);

        #[cfg(target_os = "macos")]
        {
            let mut cmd_q = KeyEvent::default();
            cmd_q.key = SharedString::new_static("q");
            cmd_q.code = SharedString::new_static("KeyQ");
            cmd_q.modifiers.cmd = true;
            assert_eq!(key_event_to_binding(&cmd_q), None);
        }
    }
}
