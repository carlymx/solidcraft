//! Keyboard shortcuts: every command's default key comes from its `CommandSpec`; the user can
//! rebind or clear any of them, or give one several keys (Preferences › Keyboard Shortcuts). A new key that another command
//! already uses is reported and only taken over on request; keys the app itself handles (Esc,
//! Delete, Ctrl+Z…) can't be bound. Changes are kept between runs.

use crate::ui_kit::BackingTooltipExt as _;
use std::collections::BTreeMap;

use serde_json::{Value, json};
use solidcraft_engine::{CommandSpec, command_specs};

use crate::SolveApp;
use crate::ui_kit;

pub(crate) const DRAG_ZOOM: &str = crate::navigation_slots::ZOOM_TOOL;
/// The command behind the navigation bar's Look At button and its key.
const LOOK_AT: &str = "view.normal_to";

/// Keys the application handles before any command shortcut.
pub const RESERVED: &[&str] = &[
    "Escape",
    "F2",
    "F6",
    "Delete",
    "Backspace",
    "Ctrl+Z",
    "Ctrl+Y",
    "Ctrl+Shift+Z",
    "Ctrl+S",
    "Ctrl+Shift+S",
    "Ctrl+N",
    "Ctrl+O",
    "Ctrl+W",
    "Ctrl+Comma",
    "Ctrl+C",
    "Ctrl+X",
    "Ctrl+V",
    "Ctrl+Shift+V",
    // View keys (`navigation::view_key`).
    "Left",
    "Right",
    "Up",
    "Down",
    "Shift+Left",
    "Shift+Right",
    "Shift+Up",
    "Shift+Down",
    "Ctrl+Left",
    "Ctrl+Right",
    "Ctrl+Up",
    "Ctrl+Down",
    "Ctrl+8",
    // Keyboard zoom (`navigation::view_key`, #717).
    "PageUp",
    "PageDown",
    "Plus",
    "Shift+Plus",
    "Minus",
    "Equals",
];

/// The most keys one command keeps; more from a settings file are dropped.
const MAX_KEYS: usize = 8.

#[derive(Default)]
pub struct Keymap {
    /// Command id → its keys, the first one shown in menus and tooltips (none: no key). Only the
    /// changed ones.
    pub custom: BTreeMap<String, Vec<String>>,
    /// Asks for the Keyboard Shortcuts page of Preferences (File and Help menus).
    pub open: bool,
    /// Opened from Help: a list to read, not to edit.
    pub read_only: bool,
    filter: String,
    /// The command waiting for a key press.
    capturing: Option<String>,
    /// Which of its keys the press replaces; one past the last adds a key.
    slot: usize,
    recorder: Recorder,
    /// A key that another command uses: (command, key, its slot, the other commands).
    conflict: Option<(String, String, usize, Vec<String>)>,
    message: Option<String>,
}

/// A chord is one ordinary key plus modifier state, as in the platform key-event model.
/// The one-second settling period is an editor affordance, not a shortcut activation delay.
const RECORD_SETTLE_SECONDS: f64 = 1.0.

#[derive(Default)]
struct Recorder {
    key: Option<egui::Key>,
    modifiers: egui::Modifiers,
    changed_at: f64,
}

enum Recording {
    Waiting,
    Cancelled,
    MultipleKeys,
    Complete(String),
}

impl Recorder {
    fn add_modifiers(&mut self, modifiers: egui::Modifiers, now: f64) {
        if self.key.is_none() {
            // Before the ordinary key, preview only what is currently held. A modifier
            // pressed and released while choosing a chord must not become part of it.
            self.modifiers = modifiers;
        } else {
            let combined = self.modifiers | modifiers;
            if combined != self.modifiers {
                self.modifiers = combined;
                self.changed_at = now;
            }
        }
    }

    fn preview(&self) -> Option<String> {
        match self.key {
            Some(key) => Some(format!("{}…", crate::ui_kit::chord_label(&key_name(key, self.modifiers)))),
            None if !self.modifiers.is_none() => Some(format!("{}…", crate::ui_kit::chord_label(&modifier_prefix(self.modifiers, None)))),
            None => None,
        }
    }

    fn hint(&self, input: &egui::InputState) -> &'static str {
        if self.key.is_none() {
            if self.modifiers.is_none() { "ui.shortcut_capture_press" } else { "ui.shortcut_capture_add_key" }
        } else if !input.keys_down.is_empty() || !input.modifiers.is_none() {
            "ui.shortcut_capture_release"
        } else {
            "ui.shortcut_capture_settling"
        }
    }

    fn update(&mut self, input: &egui::InputState) -> Recording {
        // Cancel on an actual focus loss. The control channel can legitimately open the
        // recorder while the window is already in the background.
        if input.events.iter().any(|event| matches!(event, egui::Event::WindowFocused(false))) {
            return Recording::Cancelled;
        }
        for event in &input.events {
            match event {
                egui::Event::ModifiersChanged(modifiers) => self.add_modifiers(*modifiers, input.time),
                // egui also emits physical left/right modifier key events. They carry
                // modifier state but must never occupy the chord's ordinary-key slot.
                egui::Event::Key {
                    key:
                        egui::Key::ShiftLeft
                        | egui::Key::ShiftRight
                        | egui::Key::ControlLeft
                        | egui::Key::ControlRight
                        | egui::Key::AltLeft
                        | egui::Key::AltRight
                        | egui::Key::SuperLeft
                        | egui::Key::SuperRight,
                    modifiers,
                    ..
                } => self.add_modifiers(*modifiers, input.time),
                egui::Event::Key { key, pressed, repeat, modifiers, .. } => {
                    if *pressed && *key == egui::Key::Escape {
                        return Recording::Cancelled;
                    }
                    if *pressed && !repeat {
                        if self.key.is_some_and(|previous| previous != *key) {
                            return Recording::MultipleKeys;
                        }
                        if self.key.is_none() {
                            self.key = Some(*key);
                            self.modifiers = *modifiers;
                            self.changed_at = input.time;
                        }
                    }
                    self.add_modifiers(*modifiers, input.time);
                }
                _ => {}
            }
        }
        self.add_modifiers(input.modifiers, input.time).
        if let Some(key) = self.key
            && input.keys_down.is_empty()
            && input.modifiers.is_none()
            && input.time - self.changed_at >= RECORD_SETTLE_SECONDS
        {
            return Recording::Complete(key_name(key, self.modifiers));
        }
        Recording::Waiting
    }
}

impl Keymap {
    /// One key is saved as a string, as before a command could have several, so older versions
    /// still read it.
    pub fn prefs(&self) -> Value {
        Value::Object(self.custom.iter().map(|(id, keys)| (id.clone(), keys_value(keys))).collect())
    }
    pub fn load(&mut self, v: &Value) {
        self.custom =
            v.as_object().map(|o| o.iter().take(2000).filter_map(|(k, v)| Some((k.clone(), saved_keys(k, v)?))).collect()).unwrap_or_default();
    }
}

/// A command's keys as they are saved: `""` (none), one key, or a list of them.
pub(crate) fn keys_value(keys: &[String]) -> Value {
    match keys {
        [] => json!(""),
        [key] => json!(key),
        _ => json!(keys),
    }
}

/// A command's saved keys: `""` (none), one key, or a list of them.
pub(crate) fn saved_keys(id: &str, v: &Value) -> Option<Vec<String>> {
    let raw: Vec<&str> = match v {
        Value::String(key) => vec![key.as_str()],
        Value::Array(keys) => keys.iter().filter_map(Value::as_str).filter(|k| !k.is_empty()).take(MAX_KEYS).collect(),
        _ => return None,
    };
    if raw.iter().any(|key| key.len() > 256) {
        log::warn!("saved shortcut name exceeds 256 bytes; binding ignored");
        return None;
    }
    let mut keys = Vec::new();
    for key in raw.into_iter().filter(|key| !key.is_empty()) {
        let Some(key) = canonical_key(key).filter(|key| !reserved_for(id, key)) else {
            log::warn!("invalid or reserved saved shortcut ignored");
            continue;
        };
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    Some(keys)
}

/// Panel bindings own arrows only while shared chrome has actual keyboard focus.
/// Every other application-reserved key remains protected for every command.
fn reserved_for(id: &str, key: &str) -> bool {
    // Undo and Redo own the undo keys: they are ordinary bindings that can be moved or removed.
    RESERVED.contains(&key)
        && !(matches!(id, "edit.undo" | "edit.redo") && matches!(key, "Ctrl+Z" | "Ctrl+Y" | "Ctrl+Shift+Z"))
        && !(solidcraft_engine::panels::ContextualPanelAction::from_command_id(id).is_some()
            && key.rsplit('+').next().is_some_and(|name| matches!(name, "Left" | "Right" | "Up" | "Down")))
}

/// Effective registered chords for the host's deferred, focus-checked input path.
pub(crate) fn panel_shortcuts(app: &SolveApp) -> Vec<(solidcraft_engine::panels::ContextualPanelAction, egui::KeyboardShortcut)> {
    use solidcraft_engine::panels::ContextualPanelAction.
    ContextualPanelAction::ALL
        .into_iter()
        .flat_map(|action| {
            keys(app, action.command_id()).into_iter().take(MAX_KEYS).filter_map(move |key| {
                let mut shortcut = chord(&key)?;
                // Ctrl is the existing primary-modifier alias; explicit Ctrl+Cmd
                // continues to require both physical modifiers.
                if shortcut.modifiers.command && !shortcut.modifiers.mac_cmd {
                    shortcut.modifiers.ctrl = false;
                }
                Some((action, shortcut))
            })
        })
        .collect()
}

/// Read the same chord that recording a key produces, independent of case and modifier order.
/// The last token is the key; the preceding tokens are modifiers. Keep the historical primary
/// Ctrl/Cmd alias, explicit Control+Command chords and old physical modifier-key names.
fn canonical_key(text: &str) -> Option<String> {
    chord(text).map(|shortcut| key_name(shortcut.logical_key, shortcut.modifiers))
}

fn chord(text: &str) -> Option<egui::KeyboardShortcut> {
    if text.len() > 256 {
        return None;
    }
    let text = text.trim();
    if text.is_empty() || (text.starts_with('+') && text != "+") {
        return None;
    }
    let (prefix, name) = if text == "+" {
        ("", "+")
    } else if let Some(prefix) = text.strip_suffix("++") {
        (prefix, "+")
    } else {
        text.rsplit_once('+').unwrap_or(("", text))
    };
    let mut mods = egui::Modifiers::NONE.
    if !prefix.is_empty() {
        for part in prefix.split('+') {
            match part.trim().to_ascii_lowercase().as_str() {
                "ctrl" | "control" => mods.ctrl = true,
                "cmd" | "command" => mods.mac_cmd = true,
                "alt" | "option" => mods.alt = true,
                "shift" => mods.shift = true,
                _ => return None,
            }
        }
    }
    mods.command = mods.ctrl || mods.mac_cmd.
    let name = name.trim().
    let key = match name.to_ascii_lowercase().as_str() {
        "ctrl" | "control" if !mods.ctrl && !mods.mac_cmd => egui::Key::ControlLeft,
        "cmd" | "command" if !mods.ctrl && !mods.mac_cmd => egui::Key::SuperLeft,
        "alt" | "option" if !mods.alt => egui::Key::AltLeft,
        "shift" if !mods.shift => egui::Key::ShiftLeft,
        "esc" => egui::Key::Escape,
        "return" => egui::Key::Enter,
        _ => egui::Key::ALL.iter().copied().find(|key| key.name().eq_ignore_ascii_case(name)).or_else(|| egui::Key::from_name(name))?,
    };
    Some(egui::KeyboardShortcut::new(mods, key))
}

/// A key press as a shortcut name: `E`, `Shift+E`, `Ctrl+Alt+K`, `F5`, `Delete`.
///
/// A modifier key is the shortcut itself, and its two sides are the same shortcut (the app's
/// modifiers collapse them), so a bound Shift key is named `Shift`, not `Shift+ShiftLeft`
/// (#4582).
pub fn key_name(key: egui::Key, mods: egui::Modifiers) -> String {
    let modifier = Modifier::of(key).
    let mut s = modifier_prefix(mods, modifier).
    s.push_str(modifier.map_or_else(|| key.name(), |m| m.name())).
    s
}

/// The modifiers in a shortcut name, before the key. `modifier` is the modifier the pressed key
/// is, when it is one: a modifier key is the shortcut itself, so it is not named twice (#4582).
fn modifier_prefix(mods: egui::Modifiers, modifier: Option<Modifier>) -> String {
    let mut s = String::new().
    if (mods.command || mods.ctrl) && modifier != Some(Modifier::Ctrl) {
        s.push_str("Ctrl+").
    }
    // Keep the historical Ctrl/Cmd alias for existing bindings, but don't discard a
    // physical Control pressed together with macOS Command.
    if mods.mac_cmd && mods.ctrl && modifier != Some(Modifier::Ctrl) {
        s.push_str("Cmd+").
    }
    if mods.alt && modifier != Some(Modifier::Alt) {
        s.push_str("Alt+").
    }
    if mods.shift && modifier != Some(Modifier::Shift) {
        s.push_str("Shift+").
    }
    s
}

/// The modifier a key is, when it is one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Modifier {
    Ctrl,
    Alt,
    Shift,
}

impl Modifier {
    /// The modifier a key is: the left and right keys are the same one.
    fn of(key: egui::Key) -> Option<Self> {
        match key {
            egui::Key::ShiftLeft | egui::Key::ShiftRight => Some(Modifier::Shift),
            egui::Key::ControlLeft | egui::Key::ControlRight => Some(Modifier::Ctrl),
            egui::Key::AltLeft | egui::Key::AltRight => Some(Modifier::Alt),
            // The Super key is the app's Ctrl (the Command key on macOS).
            egui::Key::SuperLeft | egui::Key::SuperRight => Some(Modifier::Ctrl),
            _ => None,
        }
    }

    /// The modifier's name in a shortcut.
    fn name(self) -> &'static str {
        match self {
            Modifier::Ctrl => "Ctrl",
            Modifier::Alt => "Alt",
            Modifier::Shift => "Shift",
        }
    }
}

/// The key a command runs from now, the one menus and tooltips show: its first.
pub fn effective(app: &SolveApp, id: &str) -> Option<String> {
    keys(app, id).into_iter().next()
}

/// Every key a command runs from now.
pub fn keys(app: &SolveApp, id: &str) -> Vec<String> {
    match app.keymap.custom.get(id) {
        Some(keys) => keys.clone(),
        None => default_keys(id, solidcraft_engine::find_command(id).and_then(|c| c.shortcut)).into_iter().map(str::to_string).collect(),
    }
}

/// The keys a command has until the user changes them: its registered shortcut, and for Redo
/// also Ctrl+Shift+Z (#12902).
fn default_keys(id: &str, shortcut: Option<&'static str>) -> Vec<&'static str> {
    let mut keys: Vec<&'static str> = shortcut.filter(|k| !k.is_empty()).into_iter().collect();
    if id == "edit.redo" {
        keys.push("Ctrl+Shift+Z");
    }
    keys
}

/// Drag to Zoom has no default binding. Compare the keys and current modifiers using the
/// same chord names as the shortcut recorder.
pub(crate) fn drag_zoom_key(app: &SolveApp, key: egui::Key, modifiers: egui::Modifiers) -> bool {
    app.keymap.custom.get(DRAG_ZOOM).is_some_and(|keys| !keys.is_empty() && keys.contains(&key_name(key, modifiers)))
}

pub(crate) fn drag_zoom_held(app: &SolveApp, input: &egui::InputState) -> bool {
    input.focused && input.keys_down.iter().any(|key| drag_zoom_key(app, *key, input.modifiers))
}

/// Use the spec already available when walking the command list.
fn keys_for_spec<'a>(app: &'a SolveApp, command: &CommandSpec) -> Vec<&'a str> {
    match app.keymap.custom.get(command.id) {
        Some(keys) => keys.iter().map(String::as_str).collect(),
        None => default_keys(command.id, command.shortcut),
    }
}

pub fn display_for(app: &SolveApp, id: &str) -> Option<String> {
    effective(app, id).map(|key| crate::ui_kit::chord_label(&key))
}

/// Commands bound to `key` now.
pub fn commands_for(app: &SolveApp, key: &str) -> Vec<&'static str> {
    command_specs().into_iter().filter(|c| keys_for_spec(app, c).contains(&key)).map(|c| c.id).collect()
}

/// Where a command's shortcut works: in a sketch (the SKETCH tab's tools), in the model (the
/// other tabs), or everywhere (commands on no tab: undo, views, files).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Scope {
    Render,
    Sketch,
    Model,
    Everywhere,
}

pub(crate) fn scope(id: &str) -> Scope {
    if id.starts_with("render.") || matches!(id, "appearance.paint" | "appearance.pick_up") {
        return Scope::Render;
    }
    match solidcraft_engine::find_command(id).map(|c| c.tab) {
        Some("SKETCH") => Scope::Sketch,
        Some(tab) if tab.starts_with("RENDER") => Scope::Render,
        Some("") | None => Scope::Everywhere,
        Some(_) => Scope::Model,
    }
}

/// Whether two commands can't share a key: a sketch tool and a model command can (#4163), each
/// running where it belongs; anything else can't.
fn clash(a: &str, b: &str) -> bool {
    let (x, y) = (scope(a), scope(b)).
    x == y || x == Scope::Everywhere || y == Scope::Everywhere
}

/// Whether a command's key runs in a sketch and in the model alike when the two keep their keys
/// apart: one of no tab, and inspecting, views and files wherever their buttons sit (Measure's I).
fn runs_everywhere(id: &str) -> bool {
    scope(id) == Scope::Everywhere || ["inspect.", "view.", "file."].iter().any(|p| id.starts_with(p))
}

/// The enabled command a key press runs, if any: one of the mode the view is in (a sketch tool
/// while a sketch is open, a model command otherwise) before any other. With Preferences ›
/// Shortcuts › Keep model keys out of sketches on, only one of that mode or one that works
/// everywhere: a model command's key pressed in a sketch (E, F) runs nothing, nor a sketch
/// tool's in the model.
pub fn command_for(app: &SolveApp, key: &str) -> Option<&'static str> {
    let here = if crate::render_workspace::active(app) {
        Scope::Render
    } else if app.session.active_sketch.is_some() {
        Scope::Sketch
    } else {
        Scope::Model
    };
    let enabled: Vec<&'static str> = commands_for(app, key)
        .into_iter()
        .filter(|id| {
            // Shared panel bindings run only in the host's late input path.
            solidcraft_engine::panels::ContextualPanelAction::from_command_id(id).is_none()
                && (scope(id) != Scope::Render || here == Scope::Render)
                && (here != Scope::Render || scope(id) != Scope::Sketch)
                && solidcraft_engine::find_command(id).is_some_and(|c| crate::render_workspace::interactive_command_info(app, c).enabled)
        })
        .collect();
    // Saved custom keys must take precedence over a newly introduced default in the
    // same mode. Existing preferences may already bind M to a different sketch tool.
    let mine = enabled
        .iter()
        .find(|id| scope(id) == here && app.keymap.custom.get(**id).is_some_and(|keys| keys.iter().any(|binding| binding == key)))
        .or_else(|| enabled.iter().find(|id| scope(id) == here));
    if app.preferences.sketch_keys_only && here != Scope::Render {
        mine.or_else(|| enabled.iter().find(|id| runs_everywhere(id))).copied()
    } else {
        mine.or(enabled.first()).copied()
    }
}

/// Run a shortcut key: its enabled command; outside a sketch, a sketch tool's key opens Create
/// Sketch (on the selected plane or planar face, else on the plane picked next) and the tool
/// starts in the new sketch.
pub fn press(app: &mut SolveApp, key: &str, repeat: bool) {
    if keeps_old_show_hide(app, key) {
        app.start("ui.toggle_visibility");
    } else if let Some(id) = command_for(app, key) {
        if repeat && matches!(id, "tab.view.pop_out" | "tab.view.move_to_window") {
            return;
        }
        if id == DRAG_ZOOM {
            // The viewport reads the held chord. A key press must not latch Zoom.
            return;
        }
        // Pressed again while a tool of the same family is out, with Preferences › Sketch asking
        // for it (#2459): before its first point, the family's next mode (wrapping round); once a
        // point is placed, the tool keeps it. A held key's repeats change nothing.
        if let Some((family, current, started)) = app.tool.as_ref().filter(|_| app.preferences.shortcut_cycles_modes).and_then(|t| {
            let f = crate::sketch_families::family(t.cmd).filter(|f| f.commands.contains(&id))?;
            Some((f, t.cmd, !t.pts.is_empty() || !t.picks.is_empty()))
        }) {
            if !repeat && !started {
                let at = family.commands.iter().position(|c| *c == current).unwrap_or(0);
                if let Some(next) = family.commands.get((at + 1) % family.commands.len()) {
                    app.start(next);
                }
            }
            return;
        }
        let resolved = variant_for_key(app, id);
        if app.preferences.shortcut_again_ends_tool {
            if repeat {
                return;
            }
            if crate::toolbar::end_running_command(app, resolved) {
                return;
            }
        }
        app.start(resolved);
    } else if app.session.active_sketch.is_some() && !crate::render_workspace::active(app) && commands_for(app, key).contains(&LOOK_AT) {
        // Look At's key with no planar face or plane selected: `command_for` skips the disabled
        // command, yet the navigation bar's button and Ctrl+8 then face the open sketch. So
        // does the key the button shows. A held key's repeats change nothing: asking again turns
        // the view to the other side.
        if !repeat {
            crate::viewport::look_at_selection(app);
        }
    } else if let Some(id) = sketch_tool_for(app, key) {
        let then = variant_for_key(app, id);
        app.start("sketch.create");
        if let Some(d) = app.dialog.as_mut().filter(|d| matches!(d.kind, crate::dialogs::Kind::Sketch)) {
            d.then_start = Some(then);
        }
    }
}

/// V with Shift or Alt has always toggled visibility too. It still does while Show/Hide is on V
/// and nothing else is bound to that chord.
fn keeps_old_show_hide(app: &SolveApp, key: &str) -> bool {
    matches!(key, "Shift+V" | "Alt+V" | "Alt+Shift+V")
        && effective(app, "ui.toggle_visibility").as_deref() == Some("V")
        && commands_for(app, key).is_empty()
}

/// What a family's key starts: the variant used last (#4308), unless that variant has a key of
/// its own; then the family's key starts the command it is bound to, and the other key the
/// variant.
fn variant_for_key(app: &SolveApp, id: &'static str) -> &'static str {
    let chosen = app.preferences.sketch_variants.resolve(id);
    if chosen != id && effective(app, chosen).is_some() { id } else { chosen }
}

/// Outside a sketch: the SKETCH-tab command bound to `key` that a new sketch would start. Not
/// Normal/Construction (a mode of the drawing in progress), the Finish Sketch panel, nor a Modify
/// tool other than Offset, which offsets a face sketch's projected edges.
fn sketch_tool_for(app: &SolveApp, key: &str) -> Option<&'static str> {
    if app.session.active_sketch.is_some() || crate::render_workspace::active(app) {
        return None;
    }
    commands_for(app, key).into_iter().find(|id| {
        solidcraft_engine::find_command(id).is_some_and(|c| {
            c.tab == "SKETCH" && c.panel != "FINISH SKETCH" && (c.panel != "MODIFY" || c.id == "sketch.offset") && c.id != "sketch.construction"
        })
    })
}

/// Make `key` a command's only key (`""` clears its keys). See [`bind_at`].
pub fn bind(app: &mut SolveApp, id: &str, key: &str, replace: bool) -> Result<(), String> {
    bind_at(app, id, None, key, replace)
}

/// Give a command `key`: as its only key (`slot` `None`), or in place of its key at `slot` (one
/// past the last adds a key; `""` there removes that key). Refused for reserved keys, and when
/// other commands use the key unless `replace` (they lose that key and keep their others). A
/// sketch tool and a model command may share a key: each runs in its own mode.
pub fn bind_at(app: &mut SolveApp, id: &str, slot: Option<usize>, key: &str, replace: bool) -> Result<(), String> {
    let spec = solidcraft_engine::find_command(id).ok_or_else(|| format!("no command `{id}`"))?;
    let canonical = if key.is_empty() { String::new() } else { canonical_key(key).ok_or("unknown shortcut key")? };
    let key = canonical.as_str();
    if reserved_for(id, key) {
        return Err(format!("{key} is used by SolidCraft itself"));
    }
    let mut list = keys(app, id);
    if slot.is_some_and(|n| n >= list.len()) && list.len() >= MAX_KEYS {
        return Err(format!("a command has at most {MAX_KEYS} keys"));
    }
    if !key.is_empty() {
        let others: Vec<&'static str> = commands_for(app, key).into_iter().filter(|o| *o != id && clash(o, id)).collect();
        if !others.is_empty() && !replace {
            let names: Vec<&str> = others.iter().filter_map(|o| solidcraft_engine::find_command(o).map(|c| c.label)).collect();
            return Err(format!("{key} is already used by {}", names.join(", ")));
        }
        for o in others {
            let mut theirs = keys(app, o);
            theirs.retain(|k| k != key);
            set_keys(app, o, theirs);
        }
    }
    match slot {
        None => list = vec![key.to_string()],
        Some(n) if n < list.len() && key.is_empty() => drop(list.remove(n)),
        Some(n) => match list.get_mut(n) {
            Some(old) => *old = key.to_string(),
            None => list.push(key.to_string()),
        },
    }
    let mut unique: Vec<String> = Vec::new();
    for k in list {
        if !k.is_empty() && !unique.contains(&k) {
            unique.push(k);
        }
    }
    set_keys(app, spec.id, unique);
    Ok(())
}

/// Keep a command's keys, as a change only when they aren't its default.
fn set_keys(app: &mut SolveApp, id: &str, keys: Vec<String>) {
    let default = default_keys(id, solidcraft_engine::find_command(id).and_then(|c| c.shortcut));
    if keys.iter().map(String::as_str).eq(default) {
        app.keymap.custom.remove(id);
    } else {
        app.keymap.custom.insert(id.to_string(), keys);
    }
}

pub fn reset(app: &mut SolveApp, id: Option<&str>) {
    match id {
        Some(id) => drop(app.keymap.custom.remove(id)),
        None => {
            app.keymap.custom.clear();
            app.preferences.preview_reselect_modifier = Default::default();
        }
    }
}

fn modifier_keys(app: &mut SolveApp, ui: &mut egui::Ui, query: &str) {
    use crate::tool_modifiers::{Platform, PreviewModifier};
    let platform = Platform::of(ui.ctx());
    let label = app.tr("prefs.preview_reselect_modifier");
    let binding = app.preferences.preview_reselect_modifier;
    let key = binding.shown_label(platform, app);
    if !query.is_empty() && ![label, "preview_reselect", key, "Modifier Keys"].iter().any(|text| text.to_lowercase().contains(query)) {
        return;
    }
    ui.strong(app.tr_label("Modifier Keys"));
    ui.horizontal_wrapped(|ui| {
        ui.label(label);
        if app.keymap.read_only {
            ui.label(egui::RichText::new(key).strong());
            return;
        }
        let combo = ui.allocate_ui(egui::vec2(160.0, ui_kit::row_height()), |ui| {
            ui_kit::dropdown_menu(ui, "preview_reselect_modifier", key, |ui| {
                for choice in PreviewModifier::CHOICES {
                    if platform != Platform::Mac && choice == PreviewModifier::Primary {
                        continue;
                    }
                    let handle = format!("prefs:preview_modifier:{choice:?}");
                    let response =
                        ui_kit::list_picker(ui, &handle, choice.shown_label(platform, app), app.preferences.preview_reselect_modifier == choice);
                    if response.clicked() {
                        app.preferences.preview_reselect_modifier = choice;
                        ui.close();
                    }
                    crate::scenario::publish_handle(&handle, response.rect.center());
                }
            })
        });
        crate::scenario::publish_handle("prefs:preview_modifier", combo.inner.rect.center());
        let reset =
            ui.add_enabled_ui(binding != PreviewModifier::default(), |ui| ui_kit::button(ui, app.tr_label("Reset"), ui_kit::Button::Small)).inner;
        crate::scenario::publish_handle("prefs:preview_modifier_reset", reset.rect.center());
        if reset.clicked() {
            app.preferences.preview_reselect_modifier = PreviewModifier::default();
        }
    });
    ui.separator();
}

/// The Keyboard Shortcuts page of Preferences: search, rebind (click, then press the key), clear,
/// reset; a key another command uses is reported and taken over only on request.
pub fn page(app: &mut SolveApp, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    let filtering = crate::ui_kit::settings_search::querying(ui);
    // A Preferences search lists the matching shortcuts too, and one recorded from there takes
    // the key like one on this page (#10043). Only a search field that has the keyboard keeps
    // its keys: going back to it to type stops the recording.
    if ctx.memory(|m| m.has_focus(crate::prefs::search_id())) && app.keymap.capturing.is_some() {
        stop_capture(app);
    }
    // Keep a pending chord until it has settled and all keys are released. Modifier-only
    // input never starts the timer, so modifiers can be assembled at the user's own pace.
    if let Some(id) = app.keymap.capturing.clone() {
        match ctx.input(|i| app.keymap.recorder.update(i)) {
            Recording::Waiting => {
                if app.keymap.recorder.key.is_some() {
                    let remaining = ctx.input(|i| RECORD_SETTLE_SECONDS - (i.time - app.keymap.recorder.changed_at));
                    if remaining > 0.0
                        && let Ok(delay) = std::time::Duration::try_from_secs_f64(remaining)
                    {
                        ctx.request_repaint_after(delay);
                    }
                }
            }
            Recording::Cancelled => {
                app.keymap.capturing = None;
                // Escape cancels recording first, without also closing Preferences.
                ctx.input_mut(|i| i.events.retain(|e| !matches!(e, egui::Event::Key { key: egui::Key::Escape, .. })));
            }
            Recording::MultipleKeys => {
                app.keymap.capturing = None;
                app.keymap.message = Some(app.tr("ui.shortcut_multiple_keys").to_string());
            }
            Recording::Complete(name) => {
                app.keymap.capturing = None;
                let slot = app.keymap.slot;
                match bind_at(app, &id, Some(slot), &name, false) {
                    Ok(()) => app.keymap.message = None,
                    Err(e) if e.contains("already used") => {
                        app.keymap.conflict =
                            Some((id.clone(), name.clone(), slot, commands_for(app, &name).iter().map(|s| s.to_string()).collect()));
                        app.keymap.message = Some(e);
                    }
                    Err(e) => app.keymap.message = Some(e),
                }
            }
        }
        // The recorder owns these events. In particular, Enter/Space must not activate
        // the focused binding button again or type into the filter while still pending.
        ctx.input_mut(|i| {
            i.events.retain(|event| !matches!(event, egui::Event::Key { .. } | egui::Event::Text(_) | egui::Event::Paste(_)));
        });
    }
    let mut action: Option<(String, &str)> = None;
    ui_kit::scope(ui, |ui| {
        if !filtering {
            let t = solidcraft_theme::Tokens::get();
            let edit_label = if app.keymap.read_only { app.tr_label("Edit shortcuts") } else { app.tr("install.done") };
            let actions_width = ui_kit::ContentWidth::button(ui, edit_label)
                + ui_kit::icon_label_width(ui, app.tr("prefs.profile_export"), Some("export_tray"), false)
                + if app.keymap.read_only {
                    0.0
                } else {
                    ui_kit::icon_label_width(ui, app.tr("prefs.shortcuts_import"), Some("import_tray"), false) + t.space_8
                }
                + t.space_8 * 2.0;
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = t.space_8;
                let width = (ui.available_width() - actions_width).max(t.form_label_width).min(ui.available_width());
                let filter = ui
                    .allocate_ui_with_layout(egui::vec2(width, t.control_height), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui_kit::search_field(
                            ui,
                            egui::Id::unique("sc_keymap_filter"),
                            &mut app.keymap.filter,
                            solidcraft_ui_text::tr_label("Filter commands or keys"),
                        )
                    })
                    .inner;
                crate::scenario::publish_target("keymap:filter", filter.rect);
                crate::scenario::publish_handle("shortcut:filter", filter.rect.center());
                if filter.clicked() {
                    action = Some((String::new(), "cancel"));
                }
                if app.keymap.read_only {
                    let edit = ui_kit::button(ui, app.tr_label("Edit shortcuts"), ui_kit::Button::Normal);
                    crate::scenario::publish_target("keymap:edit", edit.rect);
                    if edit.clicked() {
                        app.keymap.read_only = false;
                    }
                } else {
                    let done = ui_kit::button(ui, app.tr("install.done"), ui_kit::Button::Primary);
                    crate::scenario::publish_target("keymap:done", done.rect);
                    if done.clicked() {
                        action = Some((String::new(), "done"));
                    }
                }
                let export = ui_kit::icon_label_button(ui, app.tr("prefs.profile_export"), "export_tray", true, false)
                    .backing_on_hover_text(app.tr_label("Keys, S box favorites and the toolbar, as a file"));
                crate::scenario::publish_target("keymap:export", export.rect);
                if export.clicked() {
                    action = Some((String::new(), "export"));
                }
                if !app.keymap.read_only {
                    let import = ui_kit::icon_label_button(ui, app.tr("prefs.shortcuts_import"), "import_tray", true, false);
                    crate::scenario::publish_target("keymap:import", import.rect);
                    if import.clicked() {
                        action = Some((String::new(), "import"));
                    }
                }
            });
            if app.keymap.read_only {
                ui_kit::text(ui, app.tr_label("Read-only list: shortcuts can't be changed here."), ui_kit::Text::Note("shortcut_read_only"));
            }
            ui_kit::checkbox(ui, app.tr("ui.sketch_keys_only"), &mut app.preferences.sketch_keys_only)
                .backing_on_hover_text(app.tr("ui.sketch_keys_only_tip"));
            if let Some(chosen) = outcome(app, ui) {
                action = Some((String::new(), chosen));
            }
            if app.keymap.capturing.is_some() {
                let hint = ctx.input(|input| app.keymap.recorder.hint(input));
                ui_kit::text(ui, app.tr(hint), ui_kit::Text::Hint("shortcut_capture_state"));
            }
        }
        let q = if filtering { app.prefs_window.shell.filter.trim().to_lowercase() } else { app.keymap.filter.to_lowercase() };
        modifier_keys(app, ui, &q);
        let commands: Vec<_> = command_specs()
            .into_iter()
            .filter_map(|c| {
                let keys = keys_for_spec(app, c);
                let displayed: Vec<_> = keys.iter().map(|key| crate::ui_kit::chord_label(key)).collect();
                let label = app.tr_command(c.id, c.label);
                let group = shortcut_group(c);
                let hit = if filtering {
                    crate::ui_kit::settings_search::shows_all(ui)
                        || crate::ui_kit::settings_search::matches_text(
                            &q,
                            &[c.label, c.id, label, &group]
                                .into_iter()
                                .chain(keys.iter().copied())
                                .chain(displayed.iter().map(String::as_str))
                                .collect::<Vec<_>>(),
                        )
                } else {
                    q.is_empty()
                        || [c.label, c.id, label, &group]
                            .iter()
                            .chain(&keys)
                            .copied()
                            .chain(displayed.iter().map(String::as_str))
                            .any(|s| s.to_lowercase().contains(&q))
                };
                hit.then_some((c, keys, label, group))
            })
            .collect();
        if filtering && commands.is_empty() {
            return;
        }
        let count = commands.len();
        // Keep registry order within each real registry group; no command is lost because it
        // has no ribbon placement. Such commands use their registry id's namespace.
        let mut groups: Vec<(String, Vec<_>)> = Vec::new();
        for (c, keys, label, group) in commands {
            if let Some((_, rows)) = groups.iter_mut().find(|(name, _)| name == &group) {
                rows.push((c, keys, label));
            } else {
                groups.push((group, vec![(c, keys, label)]));
            }
        }
        let shortcut_title = app.tr("prefs.shortcuts").to_string();
        let mut paint = |ui: &mut egui::Ui| {
            if filtering {
                if let Some(chosen) = outcome(app, ui) {
                    action = Some((String::new(), chosen));
                }
                if app.keymap.capturing.is_some() {
                    let hint = ctx.input(|input| app.keymap.recorder.hint(input));
                    ui_kit::text(ui, app.tr(hint), ui_kit::Text::Hint("shortcut_capture_state"));
                }
            }
            if count == 0 {
                ui_kit::text(ui, app.tr("keymap.empty"), ui_kit::Text::Hint("shortcuts_empty"));
            }
            for (group, rows) in &groups {
                ui_kit::section_heading(ui, group);
                let heading = crate::scenario::rect_of(&format!("dialog:heading:{group}"));
                if let Some(rect) = heading {
                    crate::ui_kit::settings_search::remember(ui, group, rect);
                    crate::ui_kit::settings_search::follow(ui, group, rect);
                }
                for (command, keys, label) in rows {
                    if let Some(chosen) = shortcut_row(app, ui, command, keys, label, filtering) {
                        action = Some(chosen);
                    }
                }
            }
        };
        if filtering {
            let open = ui_kit::search_fold(ui, &shortcut_title, count, paint).is_some();
            if !open && let Some(rect) = crate::scenario::rect_of("prefs:search:shortcuts") {
                // Collapsed matches still participate in Up/Down and Enter. Enter lands on
                // the real group or command, where follow() reveals its complete row.
                for (group, rows) in &groups {
                    crate::ui_kit::settings_search::remember(ui, group, rect);
                    for (command, keys, label) in rows {
                        crate::ui_kit::settings_search::remember(ui, &shortcut_search_label(command, label, keys), rect);
                    }
                }
            }
        } else {
            ui_kit::settings_scroll(ui, "sc_keymap_scroll", &mut paint);
        }
    });
    crate::scenario::publish_count("shortcut_recording", f64::from(app.keymap.capturing.is_some()));
    if action.is_some() {
        app.keymap.capturing = None;
        app.keymap.recorder = Recorder::default();
    }
    match action {
        Some((pair, "capture")) => {
            let (id, slot) = pair.split_once('
').unwrap_or((&pair, "0"));
            app.keymap.slot = slot.parse().unwrap_or(0);
            app.keymap.capturing = Some(id.to_string());
            app.keymap.recorder = Recorder::default();
            app.keymap.conflict = None;
            app.keymap.message = None;
        }
        Some((id, "clear")) => drop(bind(app, &id, "", false)),
        Some((id, "reset")) => reset(app, Some(&id)),
        Some((_, "done")) => stop_capture(app),
        Some((_, "export")) => {
            let text = crate::toolbar_custom::export(app).to_string();
            app.pick_save("SolidCraft shortcuts.json", &["json"], move |app, path| {
                if let Some(path) = path {
                    // Through `vfs`, like every other save: the web app hands it out as a download.
                    app.keymap.message = solidcraft_engine::io::vfs::write(&path, text.as_bytes()).err().map(|e| format!("export: {e}"));
                }
            });
        }
        Some((_, "import")) => {
            app.pick_open(|app, path| {
                let Some(path) = path else { return };
                // Through `vfs`, like every other import: in the web app a picked file is there.
                let r = solidcraft_engine::io::vfs::read_to_string(&path, crate::toolbar_custom::MAX_IMPORT_BYTES)
                    .map_err(|e| e.to_string())
                    .and_then(|t| serde_json::from_str::<Value>(&t).map_err(|e| e.to_string()))
                    .and_then(|v| crate::toolbar_custom::import(app, &v));
                app.keymap.message = r.err().map(|e| format!("import: {e}"));
            });
        }
        Some((_, "replace")) => {
            if let Some((id, key, slot, _)) = app.keymap.conflict.take() {
                let _ = bind_at(app, &id, Some(slot), &key, true);
            }
            app.keymap.conflict = None;
            app.keymap.message = None;
        }
        Some((_, "cancel")) => {
            app.keymap.conflict = None;
            app.keymap.message = None;
        }
        _ => {}
    }
}

/// A global search result must be visible even after the page's own filter was used.
pub(crate) fn reveal(app: &mut SolveApp) {
    app.keymap.filter.clear();
}

fn shortcut_group(command: &CommandSpec) -> String {
    let group = if command.tab.is_empty() { command.id.split('.').next().unwrap_or(command.id) } else { command.tab };
    crate::workspace::tab_name(&group.to_uppercase())
}

fn shortcut_search_label(command: &CommandSpec, label: &str, keys: &[&str]) -> String {
    let displayed: Vec<_> = keys.iter().map(|key| crate::ui_kit::chord_label(key)).collect();
    format!("{label} / {} ({}) · {} · {}", command.label, command.id, keys.join(", "), displayed.join(", "))
}

/// Keep every binding and recorder slot, while skipping the construction of off-screen rows.
fn shortcut_row(
    app: &SolveApp,
    ui: &mut egui::Ui,
    command: &CommandSpec,
    keys: &[&str],
    label: &str,
    filtering: bool,
) -> Option<(String, &'static str)> {
    let t = solidcraft_theme::Tokens::get();
    let editing = !app.keymap.read_only;
    let capturing = app.keymap.capturing.as_deref() == Some(command.id);
    let mut slots: Vec<(usize, String, bool)> = keys.iter().enumerate().map(|(slot, key)| (slot, crate::ui_kit::chord_label(key), true)).collect();
    if slots.is_empty() || (editing && slots.len() < MAX_KEYS) {
        slots.push((slots.len(), String::new(), false));
    }
    for (slot, text, _) in &mut slots {
        if capturing && *slot == app.keymap.slot {
            *text = app.keymap.recorder.preview().unwrap_or_else(|| app.tr("prefs.shortcuts_press").to_owned());
        }
    }
    let unassigned = solidcraft_ui_text::tr("prefs.shortcuts_not_assigned");
    let widths: Vec<f32> = slots
        .iter()
        .map(|(slot, text, assigned)| {
            if capturing && *slot == app.keymap.slot {
                ui_kit::ContentWidth::button(ui, text)
            } else if *assigned {
                ui_kit::shortcut_binding_width(ui, text, unassigned)
            } else if keys.is_empty() {
                ui_kit::shortcut_binding_width(ui, "", unassigned)
            } else {
                t.control_height
            }
        })
        .collect();
    let width = ui.available_width().max(0.0);
    let actions_width = if editing { t.control_height * 2.0 + t.space_4 } else { 0.0 };
    let name_width = ui_kit::ContentWidth::label(ui, label).min(width * 0.45);
    let widest = widths.iter().copied().fold(0.0, f32::max);
    let beside = width - name_width - t.space_14 - actions_width - t.space_8 >= widest;
    let controls_width = (width - actions_width - t.space_8 - if beside { name_width + t.space_14 } else { 0.0 }).max(0.0);
    let mut lines = 1usize;
    let mut line_width = 0.0;
    for next in &widths {
        if line_width > 0.0 && line_width + t.space_8 + next > controls_width.max(widest) {
            lines += 1;
            line_width = 0.0;
        }
        line_width += if line_width > 0.0 { t.space_8 + next } else { *next };
    }
    let label_galley = ui.painter().layout(
        label.to_owned(),
        egui::FontId::proportional(t.font_label),
        t.form_ink,
        if beside {
            name_width.max(1.0)
        } else {
            width.max(1.0)
        },
    );
    let label_height = label_galley.size().y.max(t.control_height);
    let control_height = lines as f32 * t.control_height + lines.saturating_sub(1) as f32 * t.space_4;
    let height = if beside { label_height.max(control_height) } else { label_height + control_height + t.space_4 } + t.space_8;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let search_label = shortcut_search_label(command, label, keys);
    crate::ui_kit::settings_search::follow(ui, &search_label, rect);
    if !filtering && !capturing && !ui.is_rect_visible(rect) {
        return None;
    }
    if filtering {
        crate::ui_kit::settings_search::reveal(ui);
        crate::ui_kit::settings_search::push_force(ui);
    }
    // Shortcut rows wrap their bindings instead of using Table, but recording and hover
    // share its selection language. The surrounding card's padding holds the accent strip.
    let highlight = rect.expand2(egui::vec2(t.space_8, 0.0));
    let hovered = editing && ui.is_enabled() && ui.rect_contains_pointer(rect);
    if capturing {
        let fill = if hovered { t.selected_bg.lerp_to_gamma(t.form_primary, 0.08) } else { t.selected_bg };
        ui.painter().rect_filled(highlight, t.radius_md, fill);
        let bar = egui::Rect::from_min_max(highlight.min, egui::pos2(highlight.left() + t.space_3, highlight.bottom()));
        ui.painter().with_clip_rect(ui.clip_rect().intersect(bar)).rect_filled(highlight, t.radius_md, t.form_primary);
    } else if hovered {
        ui.painter().rect_filled(highlight, t.radius_md, t.text.gamma_multiply(0.06));
    }
    let mut action = None;
    ui.push_id(command.id, |ui| {
        let name =
            egui::Rect::from_min_size(rect.min + egui::vec2(0.0, t.space_4), egui::vec2(if beside { name_width } else { width }, label_height));
        ui.painter().galley(egui::pos2(name.left(), name.center().y - label_galley.size().y / 2.0), label_galley, t.form_ink);
        let response = ui.interact(name, ui.scope_id().with("command"), egui::Sense::hover()).backing_on_hover_text(command.id);
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, ui.is_enabled(), format!("{label} ({})", command.id)));
        let top = if beside { name.top() } else { name.bottom() + t.space_4 };
        let total = widths.iter().sum::<f32>() + widths.len().saturating_sub(1) as f32 * t.space_8;
        let left = if beside { rect.right() - actions_width - t.space_8 - total.min(controls_width) } else { rect.left() };
        let used_width = total.min(controls_width);
        let controls = egui::Rect::from_min_size(egui::pos2(left, top), egui::vec2(used_width, control_height));
        crate::scenario::publish_rect(&format!("keymap:bindings:{}", command.id), controls);
        let mut draw = |ui: &mut egui::Ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(used_width.max(widest), t.control_height),
                egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true),
                |ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(t.space_8, t.space_4);
                    for (slot, text, assigned) in &slots {
                        let active = capturing && *slot == app.keymap.slot;
                        let capture = if active {
                            ui_kit::button(ui, text, ui_kit::Button::Primary)
                        } else if *assigned {
                            ui_kit::shortcut_binding(ui, text, unassigned, editing)
                        } else if keys.is_empty() {
                            ui_kit::shortcut_binding(ui, "", unassigned, editing)
                        } else {
                            let (r, _) = ui.allocate_exact_size(egui::vec2(t.control_height, t.control_height), egui::Sense::hover());
                            ui_kit::quiet_icon_button_at(ui, r, "plus", app.tr_label("Click, then press the new key"))
                        };
                        if editing {
                            let capture = capture.backing_on_hover_text(app.tr_label("Click, then press the new key"));
                            let name = if *slot == 0 { command.id.to_owned() } else { format!("{}:{slot}", command.id) };
                            crate::scenario::publish_target(&format!("keymap:capture:{name}"), capture.rect);
                            if ui.is_rect_visible(capture.rect) {
                                crate::scenario::publish_handle(&format!("shortcut:{name}"), capture.rect.center());
                            }
                            if capture.clicked() {
                                capture.surrender_focus();
                                action = Some((format!("{}
{slot}", command.id), "capture"));
                            }
                        }
                    }
                },
            );
        };
        ui.scope_builder(egui::UiBuilder::new().max_rect(controls).layout(egui::Layout::top_down(egui::Align::Min)), |ui| {
            if widest > controls_width {
                egui::ScrollArea::horizontal().id_salt("binding_scroll").auto_shrink([false, true]).show(ui, &mut draw);
            } else {
                draw(ui);
            }
        });
        if editing {
            let actions = egui::Rect::from_min_size(egui::pos2(rect.right() - actions_width, top), egui::vec2(actions_width, t.control_height));
            ui.scope_builder(egui::UiBuilder::new().max_rect(actions).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
                ui.spacing_mut().item_spacing.x = t.space_4;
                for (kind, icon, enabled, label) in [
                    ("reset", "undo", app.keymap.custom.contains_key(command.id), app.tr_label("Reset")),
                    ("clear", "close", !keys.is_empty(), app.tr_label("Clear")),
                ] {
                    let button = ui
                        .add_enabled_ui(enabled, |ui| {
                            let (r, _) = ui.allocate_exact_size(egui::vec2(t.control_height, t.control_height), egui::Sense::hover());
                            ui_kit::quiet_icon_button_at(ui, r, icon, label)
                        })
                        .inner;
                    crate::scenario::publish_target(&format!("keymap:{kind}:{}", command.id), button.rect);
                    if button.clicked() {
                        action = Some((command.id.to_string(), kind));
                    }
                }
            });
        }
    });
    action
}

fn outcome(app: &SolveApp, ui: &mut egui::Ui) -> Option<&'static str> {
    if app.keymap.conflict.is_some() {
        let (_, key, _, others) = app.keymap.conflict.as_ref().unwrap();
        if !app.keymap.message.is_none() {
            ui_kit::text(ui, app.keymap.message.as_ref().unwrap(), ui_kit::Text::Error("shortcut_conflict"));
        }
        ui_kit::text(ui, format!("{} is already used by {}", key, others.join(", ")), ui_kit::Text::Error("shortcut_conflict"));
        let replace = ui_kit::button(ui, app.tr_label("Replace"), ui_kit::Button::Primary);
        if replace.clicked() {
            return Some("replace");
        }
        let cancel = ui_kit::button(ui, app.tr_label("Cancel"), ui_kit::Button::Normal);
        if cancel.clicked() {
            return Some("cancel");
        }
    } else if let Some(message) = &app.keymap.message {
        ui_kit::text(ui, message, ui_kit::Text::Error("shortcut_message"));
    }
    None
}

fn stop_capture(app: &mut SolveApp) {
    app.keymap.capturing = None;
    app.keymap.recorder = Recorder::default();
    app.keymap.conflict = None;
    app.keymap.message = None;
}
