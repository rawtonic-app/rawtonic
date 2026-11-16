//! The macOS menu bar, built from the webview's command table. The webview
//! owns the commands, their labels (its locale files) and their keys (the
//! shortcut table with the user's own keys, `src/utils/appMenu.ts`); it
//! sends the whole tree with `set_app_menu` and the enabled/checked state
//! with `set_app_menu_state`. A command item (`cmd:` id) only tells the
//! webview which command was chosen (`app-menu-command`), which runs the
//! handler its key runs: there is no second implementation here.
//!
//! Until the webview sent its tree the menu is `app_quit::menu`. Native
//! roles (Services, Hide, Cut/Copy/Paste for text fields, the Window menu's
//! items) are AppKit's predefined items; Quit stays `app_quit`'s, which asks
//! the editor to save first. Linux and Windows keep their current behaviour
//! (no menu bar): the commands validate the tree there and set nothing.
//!
//! Shortcuts are set on AppKit's items directly (not muda's accelerators):
//! the key is the character the active keyboard layout types on the key
//! the webview matches (`src/utils/keyboardLayout.ts`), with AppKit's own
//! re-mapping of key equivalents turned off, so the menu shows the key that
//! runs the command on a Swiss or German keyboard too (UXR4M-06). Keys
//! without ⌘ (ratings, flags, views) are shown as well (UXR4M-05); they are
//! live only while the main window is key, where the web view takes every
//! key first, so a native dialog's text field keeps its letters.
use std::collections::HashSet;

use serde::Deserialize;

/// Command items' ids start with this; the rest names the webview command.
pub const COMMAND_PREFIX: &str = "cmd:";
/// The event a chosen command item sends to the main webview.
#[cfg(target_os = "macos")]
const COMMAND_EVENT: &str = "app-menu-command";
/// More items than a menu bar could ever hold means a broken tree.
const MAX_NODES: usize = 2000;
const MAX_DEPTH: usize = 4;

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum MenuNode {
    #[serde(rename_all = "camelCase")]
    Item {
        id: String,
        label: String,
        #[serde(default)]
        shortcut: Option<Shortcut>,
        #[serde(default = "enabled_by_default")]
        enabled: bool,
        /// `Some` makes it a check item.
        #[serde(default)]
        checked: Option<bool>,
    },
    #[serde(rename_all = "camelCase")]
    Submenu {
        label: String,
        #[serde(default)]
        role: Option<SubmenuRole>,
        items: Vec<MenuNode>,
    },
    Separator,
    #[serde(rename_all = "camelCase")]
    Native {
        role: NativeRole,
        #[serde(default)]
        label: Option<String>,
    },
}

fn enabled_by_default() -> bool {
    true
}

/// A menu item's key: the character the keyboard types (`"p"`, `"ä"`,
/// `"+"`) or a named key (`"ArrowLeft"`, `"F6"`, `"Backspace"`), with the
/// modifiers held.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct Shortcut {
    pub key: String,
    #[serde(default)]
    pub command: bool,
    #[serde(default)]
    pub shift: bool,
    #[serde(default)]
    pub option: bool,
}

impl Shortcut {
    /// The same key, as AppKit compares them (letters without case).
    fn identity(&self) -> (String, bool, bool, bool) {
        (
            self.key.to_lowercase(),
            self.command,
            self.shift,
            self.option,
        )
    }
}

/// Menus AppKit treats specially (window list, help search).
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SubmenuRole {
    Window,
    Help,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum NativeRole {
    Services,
    Hide,
    HideOthers,
    ShowAll,
    /// `app_quit`'s Quit, which lets the editor save first.
    Quit,
    Cut,
    Copy,
    Paste,
    Minimize,
    Zoom,
    Fullscreen,
    BringAllToFront,
    CloseWindow,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct MenuSpec {
    pub menus: Vec<MenuNode>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ItemState {
    pub id: String,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub checked: Option<bool>,
    #[serde(default)]
    pub label: Option<String>,
}

/// The webview command a menu item id names, if it is a command item.
pub fn command_of(id: &str) -> Option<&str> {
    id.strip_prefix(COMMAND_PREFIX)
        .filter(|command| !command.is_empty())
}

/// Named keys a shortcut may use; anything else is one typed character.
const NAMED_KEYS: [&str; 13] = [
    "ArrowUp",
    "ArrowDown",
    "ArrowLeft",
    "ArrowRight",
    "Enter",
    "Tab",
    "Space",
    "Backspace",
    "Delete",
    "Escape",
    "Home",
    "End",
    "PageUp",
];

fn is_named_key(key: &str) -> bool {
    NAMED_KEYS.contains(&key)
        || key == "PageDown"
        || key
            .strip_prefix('F')
            .is_some_and(|n| n.parse::<u8>().is_ok_and(|n| (1..=24).contains(&n)))
}

/// One printable character, or a named key.
fn check_shortcut(id: &str, shortcut: &Shortcut) -> Result<(), String> {
    let mut chars = shortcut.key.chars();
    let printable = matches!((chars.next(), chars.next()), (Some(c), None) if !c.is_control() && !c.is_whitespace());
    if printable || is_named_key(&shortcut.key) {
        Ok(())
    } else {
        Err(format!(
            "item \"{id}\" has an unknown key \"{}\"",
            shortcut.key
        ))
    }
}

/// Checks a tree before anything native is built: top-level submenus, ids
/// unique and named `cmd:<command>`, shortcuts valid and unique (also
/// against AppKit's own items), labels present, one Quit.
pub fn validate(spec: &MenuSpec) -> Result<(), String> {
    struct Walk {
        ids: HashSet<String>,
        shortcuts: HashSet<(String, bool, bool, bool)>,
        nodes: usize,
        quits: usize,
    }
    fn walk(nodes: &[MenuNode], depth: usize, w: &mut Walk) -> Result<(), String> {
        if depth > MAX_DEPTH {
            return Err("menus nested too deep".into());
        }
        for node in nodes {
            w.nodes += 1;
            if w.nodes > MAX_NODES {
                return Err("too many menu items".into());
            }
            match node {
                MenuNode::Item {
                    id,
                    label,
                    shortcut,
                    ..
                } => {
                    if command_of(id).is_none() {
                        return Err(format!(
                            "item id \"{id}\" is not a {COMMAND_PREFIX} command"
                        ));
                    }
                    if label.trim().is_empty() {
                        return Err(format!("item \"{id}\" has no label"));
                    }
                    if !w.ids.insert(id.clone()) {
                        return Err(format!("item id \"{id}\" appears twice"));
                    }
                    if let Some(shortcut) = shortcut {
                        check_shortcut(id, shortcut)?;
                        if !w.shortcuts.insert(shortcut.identity()) {
                            return Err(format!("the key of \"{id}\" is on two items"));
                        }
                    }
                }
                MenuNode::Submenu { label, items, .. } => {
                    if label.trim().is_empty() {
                        return Err("a submenu has no label".into());
                    }
                    walk(items, depth + 1, w)?;
                }
                MenuNode::Native { role, .. } => {
                    if *role == NativeRole::Quit {
                        w.quits += 1;
                    }
                    if let Some(shortcut) = native_shortcut(*role)
                        && !w.shortcuts.insert(shortcut.identity())
                    {
                        return Err(format!("the key of {role:?} is on two items"));
                    }
                }
                MenuNode::Separator => {}
            }
        }
        Ok(())
    }
    if spec.menus.is_empty() {
        return Err("no menus".into());
    }
    if !spec
        .menus
        .iter()
        .all(|node| matches!(node, MenuNode::Submenu { .. }))
    {
        return Err("the menu bar holds only submenus".into());
    }
    let mut w = Walk {
        ids: HashSet::new(),
        shortcuts: HashSet::new(),
        nodes: 0,
        quits: 0,
    };
    walk(&spec.menus, 1, &mut w)?;
    if w.quits != 1 {
        return Err(format!(
            "the menu needs exactly one Quit, found {}",
            w.quits
        ));
    }
    Ok(())
}

/// The key AppKit's predefined item takes, so a command cannot take it too
/// (Full Screen's ⌃⌘F holds ⌃, which no command key does).
fn native_shortcut(role: NativeRole) -> Option<Shortcut> {
    let (key, option) = match role {
        NativeRole::Hide => ("h", false),
        NativeRole::HideOthers => ("h", true),
        NativeRole::Quit => ("q", false),
        NativeRole::Cut => ("x", false),
        NativeRole::Copy => ("c", false),
        NativeRole::Paste => ("v", false),
        NativeRole::Minimize => ("m", false),
        NativeRole::CloseWindow => ("w", false),
        NativeRole::Fullscreen
        | NativeRole::Services
        | NativeRole::ShowAll
        | NativeRole::Zoom
        | NativeRole::BringAllToFront => return None,
    };
    Some(Shortcut {
        key: key.into(),
        command: true,
        shift: false,
        option,
    })
}

#[cfg(target_os = "macos")]
mod native {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use tauri::menu::{
        CheckMenuItem, HELP_SUBMENU_ID, Menu, MenuItem, MenuItemKind, PredefinedMenuItem, Submenu,
        WINDOW_SUBMENU_ID,
    };
    use tauri::{AppHandle, Runtime};

    use super::{MenuNode, NativeRole, SubmenuRole};

    /// The command items of the menu shown, for state updates.
    pub(super) struct Items<R: Runtime>(pub Mutex<HashMap<String, MenuItemKind<R>>>);

    pub(super) fn build<R: Runtime>(
        app: &AppHandle<R>,
        menus: &[MenuNode],
        items: &mut HashMap<String, MenuItemKind<R>>,
    ) -> tauri::Result<Menu<R>> {
        let menu = Menu::new(app)?;
        let mut serial = 0;
        for node in menus {
            if let MenuNode::Submenu {
                label,
                role,
                items: children,
            } = node
            {
                let submenu = submenu(app, label, *role, children, items, &mut serial)?;
                menu.append(&submenu)?;
            }
        }
        Ok(menu)
    }

    fn submenu<R: Runtime>(
        app: &AppHandle<R>,
        label: &str,
        role: Option<SubmenuRole>,
        children: &[MenuNode],
        items: &mut HashMap<String, MenuItemKind<R>>,
        serial: &mut usize,
    ) -> tauri::Result<Submenu<R>> {
        *serial += 1;
        let id = match role {
            Some(SubmenuRole::Window) => WINDOW_SUBMENU_ID.to_string(),
            Some(SubmenuRole::Help) => HELP_SUBMENU_ID.to_string(),
            None => format!("rawtonic-menu-{serial}"),
        };
        let submenu = Submenu::with_id(app, id, label, true)?;
        for child in children {
            match child {
                MenuNode::Item {
                    id,
                    label,
                    enabled,
                    checked,
                    ..
                } => {
                    // The shortcut is set on AppKit's item (`shortcuts`).
                    let accelerator: Option<&str> = None;
                    let kind = match checked {
                        Some(checked) => MenuItemKind::Check(CheckMenuItem::with_id(
                            app,
                            id,
                            label,
                            *enabled,
                            *checked,
                            accelerator,
                        )?),
                        None => MenuItemKind::MenuItem(MenuItem::with_id(
                            app,
                            id,
                            label,
                            *enabled,
                            accelerator,
                        )?),
                    };
                    match &kind {
                        MenuItemKind::Check(item) => submenu.append(item)?,
                        MenuItemKind::MenuItem(item) => submenu.append(item)?,
                        _ => {}
                    }
                    items.insert(id.clone(), kind);
                }
                MenuNode::Submenu {
                    label,
                    role,
                    items: nested,
                } => {
                    let nested = self::submenu(app, label, *role, nested, items, serial)?;
                    submenu.append(&nested)?;
                }
                MenuNode::Separator => submenu.append(&PredefinedMenuItem::separator(app)?)?,
                MenuNode::Native {
                    role: NativeRole::Quit,
                    label,
                } => {
                    let label = label
                        .clone()
                        .unwrap_or_else(|| format!("Quit {}", app.package_info().name));
                    submenu.append(&crate::app_quit::quit_item(app, &label)?)?;
                }
                MenuNode::Native { role, label } => {
                    submenu.append(&predefined(app, *role, label.as_deref())?)?;
                }
            }
        }
        Ok(submenu)
    }

    fn predefined<R: Runtime>(
        app: &AppHandle<R>,
        role: NativeRole,
        label: Option<&str>,
    ) -> tauri::Result<PredefinedMenuItem<R>> {
        match role {
            NativeRole::Services => PredefinedMenuItem::services(app, label),
            NativeRole::Hide => PredefinedMenuItem::hide(app, label),
            NativeRole::HideOthers => PredefinedMenuItem::hide_others(app, label),
            NativeRole::ShowAll => PredefinedMenuItem::show_all(app, label),
            NativeRole::Cut => PredefinedMenuItem::cut(app, label),
            NativeRole::Copy => PredefinedMenuItem::copy(app, label),
            NativeRole::Paste => PredefinedMenuItem::paste(app, label),
            NativeRole::Minimize => PredefinedMenuItem::minimize(app, label),
            NativeRole::Zoom => PredefinedMenuItem::maximize(app, label),
            NativeRole::Fullscreen => PredefinedMenuItem::fullscreen(app, label),
            NativeRole::BringAllToFront => PredefinedMenuItem::bring_all_to_front(app, label),
            NativeRole::CloseWindow => PredefinedMenuItem::close_window(app, label),
            NativeRole::Quit => unreachable!("Quit is app_quit's item"),
        }
    }

    /// The menu items' shortcuts, set on AppKit's items once the menu shows.
    pub(super) mod shortcuts {
        use std::cell::RefCell;
        use std::sync::atomic::{AtomicBool, Ordering};

        use objc2::rc::Retained;
        use objc2::runtime::{AnyObject, Bool};
        use objc2::{class, msg_send, sel};
        use objc2_foundation::NSString;

        use super::super::{MenuNode, Shortcut};

        const SHIFT: usize = 1 << 17;
        const OPTION: usize = 1 << 19;
        const COMMAND: usize = 1 << 20;

        /// Whether the main window is key: only then does the web view take
        /// every key before the menu (single keys are live only then).
        static MAIN_WINDOW_KEY: AtomicBool = AtomicBool::new(true);

        thread_local! {
            /// Items whose key holds no ⌘, with the key equivalent they show.
            static SINGLE_KEYS: RefCell<Vec<(Retained<AnyObject>, String)>> = const { RefCell::new(Vec::new()) };
        }

        /// AppKit's key equivalent for a shortcut: the typed character or
        /// the function-key character of a named key.
        pub(crate) fn key_equivalent(shortcut: &Shortcut) -> String {
            let named = match shortcut.key.as_str() {
                "ArrowUp" => Some('\u{F700}'),
                "ArrowDown" => Some('\u{F701}'),
                "ArrowLeft" => Some('\u{F702}'),
                "ArrowRight" => Some('\u{F703}'),
                "Enter" => Some('\r'),
                "Tab" => Some('\t'),
                "Space" => Some(' '),
                "Backspace" => Some('\u{8}'),
                "Delete" => Some('\u{F728}'),
                "Escape" => Some('\u{1b}'),
                "Home" => Some('\u{F729}'),
                "End" => Some('\u{F72B}'),
                "PageUp" => Some('\u{F72C}'),
                "PageDown" => Some('\u{F72D}'),
                key => key
                    .strip_prefix('F')
                    .and_then(|n| n.parse::<u32>().ok())
                    .filter(|n| (1..=24).contains(n))
                    .and_then(|n| char::from_u32(0xF704 + n - 1)),
            };
            named.map_or_else(|| shortcut.key.to_lowercase(), String::from)
        }

        fn modifier_mask(shortcut: &Shortcut) -> usize {
            (if shortcut.command { COMMAND } else { 0 })
                | (if shortcut.shift { SHIFT } else { 0 })
                | (if shortcut.option { OPTION } else { 0 })
        }

        /// Sets the tree's shortcuts on the menu bar AppKit shows. Main
        /// thread, after `set_menu` (items are found by their titles).
        pub(crate) fn apply(menus: &[MenuNode]) {
            // SAFETY: main thread; NSMenu/NSMenuItem messages with their
            // documented argument types.
            unsafe {
                let ns_app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
                let main: *mut AnyObject = msg_send![ns_app, mainMenu];
                if main.is_null() {
                    return;
                }
                let mut single = Vec::new();
                for node in menus {
                    if let MenuNode::Submenu { label, items, .. } = node {
                        apply_submenu(main, label, items, &mut single);
                    }
                }
                let live = MAIN_WINDOW_KEY.load(Ordering::Relaxed);
                for (item, key) in &single {
                    set_key(item, if live { key } else { "" });
                }
                SINGLE_KEYS.with(|keys| *keys.borrow_mut() = single);
            }
        }

        unsafe fn item_titled(menu: *mut AnyObject, title: &str) -> *mut AnyObject {
            let title = NSString::from_str(title);
            unsafe { msg_send![menu, itemWithTitle: &*title] }
        }

        unsafe fn set_key(item: &AnyObject, key: &str) {
            let key = NSString::from_str(key);
            unsafe {
                let () = msg_send![item, setKeyEquivalent: &*key];
            }
        }

        unsafe fn apply_submenu(
            parent: *mut AnyObject,
            label: &str,
            nodes: &[MenuNode],
            single: &mut Vec<(Retained<AnyObject>, String)>,
        ) {
            unsafe {
                let holder = item_titled(parent, label);
                if holder.is_null() {
                    return;
                }
                let menu: *mut AnyObject = msg_send![holder, submenu];
                if menu.is_null() {
                    return;
                }
                for node in nodes {
                    match node {
                        MenuNode::Submenu { label, items, .. } => {
                            apply_submenu(menu, label, items, single)
                        }
                        MenuNode::Item {
                            label,
                            shortcut: Some(shortcut),
                            ..
                        } => {
                            let item = item_titled(menu, label);
                            let Some(item) = Retained::retain(item) else {
                                continue;
                            };
                            let key = key_equivalent(shortcut);
                            set_key(&item, &key);
                            let () = msg_send![&*item, setKeyEquivalentModifierMask: modifier_mask(shortcut)];
                            // macOS 12+: AppKit would otherwise move the key
                            // to another place on ISO layouts than the one
                            // the web view matches.
                            let localizes: Bool = msg_send![&*item, respondsToSelector: sel!(setAllowsAutomaticKeyEquivalentLocalization:)];
                            if localizes.as_bool() {
                                let () = msg_send![&*item, setAllowsAutomaticKeyEquivalentLocalization: Bool::NO];
                            }
                            if !shortcut.command {
                                single.push((item, key));
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        /// The main window became key or stopped being key (a sheet, a
        /// native dialog, the second window or another app): keys without ⌘
        /// go live or quiet, so a native text field keeps its letters.
        pub(crate) fn main_window_key(key: bool) {
            MAIN_WINDOW_KEY.store(key, Ordering::Relaxed);
            SINGLE_KEYS.with(|keys| {
                for (item, shown) in keys.borrow().iter() {
                    // SAFETY: main thread (window events), live items.
                    unsafe { set_key(item, if key { shown } else { "" }) };
                }
            });
        }
    }

    /// Text-editing actions sent to the first responder (the focused text
    /// field): the menu's Undo, Redo and Select All are the app's commands,
    /// and the webview hands them back here while a text field has the focus.
    pub(super) fn send_text_action<R: Runtime>(
        app: &AppHandle<R>,
        action: &str,
    ) -> Result<(), String> {
        use objc2::runtime::{AnyObject, Bool, Sel};
        use objc2::{class, msg_send, sel};
        let selector: Sel = match action {
            "undo" => sel!(undo:),
            "redo" => sel!(redo:),
            "selectAll" => sel!(selectAll:),
            _ => return Err(format!("unknown text action \"{action}\"")),
        };
        app.run_on_main_thread(move || unsafe {
            let ns_app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let nil: *mut AnyObject = std::ptr::null_mut();
            let _: Bool = msg_send![ns_app, sendAction: selector, to: nil, from: nil];
        })
        .map_err(|e| e.to_string())
    }
}

/// Replaces the menu bar with the webview's tree (macOS); validates it
/// everywhere. A tree that fails keeps the menu shown.
#[tauri::command]
pub fn set_app_menu(app: tauri::AppHandle, spec: MenuSpec) -> Result<(), String> {
    validate(&spec)?;
    #[cfg(target_os = "macos")]
    {
        use tauri::Manager;
        let mut items = std::collections::HashMap::new();
        let menu = native::build(&app, &spec.menus, &mut items).map_err(|e| e.to_string())?;
        app.set_menu(menu).map_err(|e| e.to_string())?;
        // Queued after set_menu's own main-thread work: the items exist.
        let menus = spec.menus.clone();
        app.run_on_main_thread(move || native::shortcuts::apply(&menus))
            .map_err(|e| e.to_string())?;
        match app.try_state::<native::Items<tauri::Wry>>() {
            Some(state) => *state.0.lock().unwrap_or_else(|e| e.into_inner()) = items,
            None => {
                app.manage(native::Items(std::sync::Mutex::new(items)));
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = app;
    Ok(())
}

/// Sets command labels and enabled/checked state; unknown ids in a tree
/// being replaced are skipped.
#[tauri::command]
pub fn set_app_menu_state(app: tauri::AppHandle, items: Vec<ItemState>) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        use tauri::Manager;
        use tauri::menu::MenuItemKind;
        let Some(state) = app.try_state::<native::Items<tauri::Wry>>() else {
            return Ok(());
        };
        let shown = state.0.lock().unwrap_or_else(|e| e.into_inner());
        for update in &items {
            match shown.get(&update.id) {
                Some(MenuItemKind::MenuItem(item)) => {
                    if let Some(label) = &update.label {
                        item.set_text(label).map_err(|e| e.to_string())?;
                    }
                    if let Some(enabled) = update.enabled {
                        item.set_enabled(enabled).map_err(|e| e.to_string())?;
                    }
                }
                Some(MenuItemKind::Check(item)) => {
                    if let Some(enabled) = update.enabled {
                        item.set_enabled(enabled).map_err(|e| e.to_string())?;
                    }
                    if let Some(checked) = update.checked {
                        item.set_checked(checked).map_err(|e| e.to_string())?;
                    }
                }
                _ => {}
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, items);
    Ok(())
}

/// Undo, Redo or Select All for the focused text field (macOS).
#[tauri::command]
pub fn app_menu_text_action(app: tauri::AppHandle, action: String) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    return native::send_text_action(&app, &action);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, action);
        Ok(())
    }
}

/// The main window gained or lost the key focus (lib.rs's window events).
#[cfg(target_os = "macos")]
pub fn main_window_focused<R: tauri::Runtime>(app: &tauri::AppHandle<R>, focused: bool) {
    let _ = app.run_on_main_thread(move || native::shortcuts::main_window_key(focused));
}

/// A menu item was chosen: a command goes to the webview, the rest is
/// `app_quit`'s (Quit, Report a Problem of the menu shown before the tree).
#[cfg(target_os = "macos")]
pub fn on_menu_event<R: tauri::Runtime>(app: &tauri::AppHandle<R>, event: tauri::menu::MenuEvent) {
    use tauri::Emitter;
    match command_of(event.id().as_ref()) {
        Some(command) => {
            if let Err(e) = app.emit_to("main", COMMAND_EVENT, command) {
                log::warn!("Could not send the menu command {command} to the editor: {e}");
            }
        }
        None => crate::app_quit::on_menu_event(app, event),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(key: &str, command: bool) -> Shortcut {
        Shortcut {
            key: key.into(),
            command,
            shift: false,
            option: false,
        }
    }

    fn item(id: &str, shortcut: Option<Shortcut>) -> MenuNode {
        MenuNode::Item {
            id: id.into(),
            label: id.into(),
            shortcut,
            enabled: true,
            checked: None,
        }
    }

    fn spec(items: Vec<MenuNode>) -> MenuSpec {
        let mut app = vec![MenuNode::Native {
            role: NativeRole::Quit,
            label: None,
        }];
        app.extend(items);
        MenuSpec {
            menus: vec![MenuNode::Submenu {
                label: "Rawtonic".into(),
                role: None,
                items: app,
            }],
        }
    }

    /// The tree the webview builds from its command table (English, default
    /// keys), written by `src/utils/appMenu.test.ts`; that test fails when
    /// the table and this file drift apart.
    const WEBVIEW_TREE: &str = include_str!("../tests/fixtures/app-menu-en.json");

    fn walk<'a>(nodes: &'a [MenuNode], out: &mut Vec<&'a MenuNode>) {
        for node in nodes {
            out.push(node);
            if let MenuNode::Submenu { items, .. } = node {
                walk(items, out);
            }
        }
    }

    #[test]
    fn the_webview_tree_is_valid_and_every_item_names_a_command() {
        let spec: MenuSpec = serde_json::from_str(WEBVIEW_TREE).unwrap();
        validate(&spec).unwrap();
        let mut nodes = Vec::new();
        walk(&spec.menus, &mut nodes);
        let mut commands = HashSet::new();
        for node in &nodes {
            if let MenuNode::Item { id, .. } = node {
                let command = command_of(id).expect("a command item");
                assert!(commands.insert(command), "{command} twice");
            }
        }
        // The menus the task names, in the macOS order, with the window and
        // help menus marked for AppKit.
        let titles: Vec<_> = spec
            .menus
            .iter()
            .map(|node| match node {
                MenuNode::Submenu { label, role, .. } => (label.as_str(), *role),
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(
            titles,
            [
                ("Rawtonic", None),
                ("File", None),
                ("Edit", None),
                ("Photo", None),
                ("View", None),
                ("Window", Some(SubmenuRole::Window)),
                ("Help", Some(SubmenuRole::Help)),
            ]
        );
        for command in [
            "undo",
            "redo",
            "select_all",
            "rate_3",
            "toggle_split_view",
            "report_problem",
        ] {
            assert!(commands.contains(command), "{command} missing");
        }
        // Text fields keep Cut/Copy/Paste; the system full screen stays.
        for role in [
            NativeRole::Cut,
            NativeRole::Copy,
            NativeRole::Paste,
            NativeRole::Fullscreen,
        ] {
            assert!(
                nodes
                    .iter()
                    .any(|n| matches!(n, MenuNode::Native { role: r, .. } if *r == role))
            );
        }
    }

    #[test]
    fn live_submenu_commands_keep_unique_ids_and_dispatch_their_registered_name() {
        let recent = "recent:/scratch/Project";
        let recipe = "recipe:web-jpeg";
        let tree = spec(vec![
            MenuNode::Submenu {
                label: "Open Recent".into(),
                role: None,
                items: vec![item(&format!("cmd:{recent}"), None)],
            },
            MenuNode::Submenu {
                label: "Export with Recipe".into(),
                role: None,
                items: vec![item(&format!("cmd:{recipe}"), None)],
            },
        ]);
        validate(&tree).unwrap();
        assert_eq!(command_of(&format!("cmd:{recent}")), Some(recent));
        assert_eq!(command_of(&format!("cmd:{recipe}")), Some(recipe));
        let mut duplicate = tree;
        if let MenuNode::Submenu { items, .. } = &mut duplicate.menus[0] {
            items.push(item(&format!("cmd:{recipe}"), None));
        }
        assert!(validate(&duplicate).unwrap_err().contains("twice"));
    }

    #[test]
    fn every_key_is_shown_single_keys_and_layout_characters_included() {
        let spec: MenuSpec = serde_json::from_str(WEBVIEW_TREE).unwrap();
        let mut nodes = Vec::new();
        walk(&spec.menus, &mut nodes);
        let shortcut_of = |command: &str| {
            nodes.iter().find_map(|n| match n {
                MenuNode::Item { id, shortcut, .. } if id == &format!("cmd:{command}") => {
                    shortcut.clone()
                }
                _ => None,
            })
        };
        // Ratings and flags show their single key (UXR4M-05).
        assert_eq!(shortcut_of("rate_3"), Some(key("3", false)));
        assert_eq!(shortcut_of("flag_pick"), Some(key("p", false)));
        assert_eq!(shortcut_of("undo"), Some(key("z", true)));
        // A layout's own characters and named keys are valid keys.
        for valid in ["ä", "+", "¨", "ArrowLeft", "F6", "Backspace", "PageDown"] {
            check_shortcut("cmd:x", &key(valid, false)).unwrap();
        }
        for invalid in ["", "ab", " ", "\t", "F25", "Plus"] {
            assert!(
                check_shortcut("cmd:x", &key(invalid, true)).is_err(),
                "{invalid:?}"
            );
        }
        #[cfg(target_os = "macos")]
        {
            use native::shortcuts::key_equivalent;
            assert_eq!(key_equivalent(&key("P", false)), "p");
            assert_eq!(key_equivalent(&key("ä", true)), "ä");
            assert_eq!(key_equivalent(&key("F6", false)), "\u{F709}");
            assert_eq!(key_equivalent(&key("ArrowLeft", false)), "\u{F702}");
            assert_eq!(key_equivalent(&key("Backspace", false)), "\u{8}");
        }
    }

    #[test]
    fn broken_trees_are_refused() {
        assert!(
            validate(&spec(vec![item("cmd:a", None), item("cmd:a", None)]))
                .unwrap_err()
                .contains("twice")
        );
        assert!(
            validate(&spec(vec![item("a", None)]))
                .unwrap_err()
                .contains("not a cmd:")
        );
        assert!(validate(&spec(vec![item("cmd:", None)])).is_err());
        assert!(
            validate(&spec(vec![
                item("cmd:a", Some(key("j", true))),
                item("cmd:b", Some(key("J", true)))
            ]))
            .unwrap_err()
            .contains("two items")
        );
        // A command cannot take a key AppKit's own item holds.
        assert!(
            validate(&spec(vec![
                item("cmd:a", Some(key("c", true))),
                MenuNode::Native {
                    role: NativeRole::Copy,
                    label: None
                }
            ]))
            .is_err()
        );
        let no_quit = MenuSpec {
            menus: vec![MenuNode::Submenu {
                label: "File".into(),
                role: None,
                items: vec![item("cmd:a", None)],
            }],
        };
        assert!(validate(&no_quit).unwrap_err().contains("Quit"));
        let loose = MenuSpec {
            menus: vec![item("cmd:a", None)],
        };
        assert!(validate(&loose).is_err());
        let mut deep = item("cmd:deep", None);
        for _ in 0..MAX_DEPTH {
            deep = MenuNode::Submenu {
                label: "x".into(),
                role: None,
                items: vec![deep],
            };
        }
        assert!(validate(&spec(vec![deep])).unwrap_err().contains("deep"));
        validate(&spec(vec![
            item("cmd:a", Some(key("j", true))),
            item("cmd:b", Some(key("j", false))),
            MenuNode::Separator,
        ]))
        .unwrap();
    }

    #[test]
    fn state_updates_and_trees_parse_from_the_webview_shape() {
        let states: Vec<ItemState> = serde_json::from_str(
            r#"[{"id":"cmd:undo","enabled":false,"label":"Undo Set Rating"},{"id":"cmd:view_develop","checked":true}]"#,
        )
        .unwrap();
        assert_eq!(
            states[0],
            ItemState {
                id: "cmd:undo".into(),
                enabled: Some(false),
                checked: None,
                label: Some("Undo Set Rating".into()),
            }
        );
        assert_eq!(states[1].checked, Some(true));
        assert_eq!(states[1].label, None);
        let node: MenuNode =
            serde_json::from_str(r#"{"kind":"item","id":"cmd:x","label":"X"}"#).unwrap();
        assert!(matches!(
            node,
            MenuNode::Item {
                enabled: true,
                checked: None,
                shortcut: None,
                ..
            }
        ));
        assert_eq!(command_of("cmd:rate_3"), Some("rate_3"));
        assert_eq!(command_of("rawtonic-quit"), None);
    }
}
