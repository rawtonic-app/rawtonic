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
        accelerator: Option<String>,
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
}

/// The webview command a menu item id names, if it is a command item.
pub fn command_of(id: &str) -> Option<&str> {
    id.strip_prefix(COMMAND_PREFIX)
        .filter(|command| !command.is_empty())
}

/// Modifiers a key equivalent may carry, as `muda` parses them.
const MODIFIERS: [&str; 8] = [
    "CmdOrCtrl",
    "CommandOrControl",
    "Cmd",
    "Command",
    "Ctrl",
    "Control",
    "Shift",
    "Alt",
];

/// Whether `code` names one key the way `muda` parses it (W3C codes).
fn is_key_code(code: &str) -> bool {
    let tail_is = |prefix: &str, ok: fn(&str) -> bool| code.strip_prefix(prefix).is_some_and(ok);
    tail_is("Key", |c| {
        c.len() == 1 && c.bytes().all(|b| b.is_ascii_uppercase())
    }) || tail_is("Digit", |c| {
        c.len() == 1 && c.bytes().all(|b| b.is_ascii_digit())
    }) || tail_is("F", |c| {
        c.parse::<u8>().is_ok_and(|n| (1..=24).contains(&n))
    }) || matches!(
        code,
        "ArrowUp"
            | "ArrowDown"
            | "ArrowLeft"
            | "ArrowRight"
            | "Enter"
            | "Tab"
            | "Space"
            | "Backspace"
            | "Delete"
            | "Escape"
            | "Equal"
            | "Minus"
            | "Comma"
            | "Period"
            | "Slash"
            | "Backslash"
            | "BracketLeft"
            | "BracketRight"
            | "Semicolon"
            | "Quote"
            | "Backquote"
            | "Home"
            | "End"
            | "PageUp"
            | "PageDown"
    )
}

/// A key equivalent must hold ⌘ or ⌃: a plain, ⇧- or ⌥-only key would take
/// the keys a text field types (single-key shortcuts stay in the webview).
fn check_accelerator(accelerator: &str) -> Result<(), String> {
    let mut parts: Vec<&str> = accelerator.split('+').collect();
    let key = parts.pop().unwrap_or_default();
    if !is_key_code(key) {
        return Err(format!("unknown key in \"{accelerator}\""));
    }
    let mut seen = HashSet::new();
    for part in &parts {
        if !MODIFIERS.contains(part) || !seen.insert(*part) {
            return Err(format!("bad modifier in \"{accelerator}\""));
        }
    }
    if !parts.iter().any(|part| !matches!(*part, "Shift" | "Alt")) {
        return Err(format!("\"{accelerator}\" holds neither Cmd nor Ctrl"));
    }
    Ok(())
}

/// Checks a tree before anything native is built: top-level submenus, ids
/// unique and named `cmd:<command>`, key equivalents valid, unique and with
/// ⌘/⌃, labels present, one Quit.
pub fn validate(spec: &MenuSpec) -> Result<(), String> {
    struct Walk {
        ids: HashSet<String>,
        accelerators: HashSet<String>,
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
                    accelerator,
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
                    if let Some(accelerator) = accelerator {
                        check_accelerator(accelerator)?;
                        if !w.accelerators.insert(accelerator.clone()) {
                            return Err(format!("key \"{accelerator}\" is on two items"));
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
                    if let Some(accelerator) = native_accelerator(*role)
                        && !w.accelerators.insert(accelerator.into())
                    {
                        return Err(format!("key \"{accelerator}\" is on two items"));
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
        accelerators: HashSet::new(),
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

/// The key AppKit's predefined item takes, so a command cannot take it too.
fn native_accelerator(role: NativeRole) -> Option<&'static str> {
    match role {
        NativeRole::Hide => Some("CmdOrCtrl+KeyH"),
        NativeRole::HideOthers => Some("CmdOrCtrl+Alt+KeyH"),
        NativeRole::Quit => Some("CmdOrCtrl+KeyQ"),
        NativeRole::Cut => Some("CmdOrCtrl+KeyX"),
        NativeRole::Copy => Some("CmdOrCtrl+KeyC"),
        NativeRole::Paste => Some("CmdOrCtrl+KeyV"),
        NativeRole::Minimize => Some("CmdOrCtrl+KeyM"),
        NativeRole::Fullscreen => Some("Ctrl+CmdOrCtrl+KeyF"),
        NativeRole::CloseWindow => Some("CmdOrCtrl+KeyW"),
        NativeRole::Services
        | NativeRole::ShowAll
        | NativeRole::Zoom
        | NativeRole::BringAllToFront => None,
    }
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
                    accelerator,
                    enabled,
                    checked,
                } => {
                    let accelerator = accelerator.as_deref();
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

/// Sets enabled/checked of the shown menu's command items; unknown ids
/// (a tree being replaced) are skipped.
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

    fn item(id: &str, accelerator: Option<&str>) -> MenuNode {
        MenuNode::Item {
            id: id.into(),
            label: id.into(),
            accelerator: accelerator.map(Into::into),
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
    fn single_key_shortcuts_never_become_key_equivalents() {
        let spec: MenuSpec = serde_json::from_str(WEBVIEW_TREE).unwrap();
        let mut nodes = Vec::new();
        walk(&spec.menus, &mut nodes);
        let rated = nodes.iter().find_map(|n| match n {
            MenuNode::Item {
                id, accelerator, ..
            } if id == "cmd:rate_3" => Some(accelerator),
            _ => None,
        });
        assert_eq!(rated, Some(&None));
        // Plain, ⇧-only and ⌥-only shortcuts.
        for shortcut in [
            ["", "KeyF"],
            ["Shift+", "Digit1"],
            ["Alt+", "KeyM"],
            ["Shift+Alt+", "KeyM"],
        ] {
            let shortcut = shortcut.concat();
            assert!(check_accelerator(&shortcut).is_err(), "{shortcut}");
        }
        for accelerator in [
            "CmdOrCtrl+KeyZ",
            "CmdOrCtrl+Shift+KeyE",
            "CmdOrCtrl+Alt+KeyV",
            "CmdOrCtrl+Equal",
            "Ctrl+CmdOrCtrl+KeyF",
            "CmdOrCtrl+Digit0",
        ] {
            check_accelerator(accelerator).unwrap();
        }
        for accelerator in [
            "CmdOrCtrl+",
            "CmdOrCtrl+Hyper+KeyA",
            "CmdOrCtrl+CmdOrCtrl+KeyA",
            "CmdOrCtrl+Plus",
        ] {
            assert!(check_accelerator(accelerator).is_err(), "{accelerator}");
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
                item("cmd:a", Some("CmdOrCtrl+KeyJ")),
                item("cmd:b", Some("CmdOrCtrl+KeyJ"))
            ]))
            .unwrap_err()
            .contains("two items")
        );
        // A command cannot take a key AppKit's own item holds.
        assert!(
            validate(&spec(vec![
                item("cmd:a", Some("CmdOrCtrl+KeyC")),
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
            item("cmd:a", Some("CmdOrCtrl+KeyJ")),
            MenuNode::Separator,
        ]))
        .unwrap();
    }

    #[test]
    fn state_updates_and_trees_parse_from_the_webview_shape() {
        let states: Vec<ItemState> = serde_json::from_str(
            r#"[{"id":"cmd:undo","enabled":false},{"id":"cmd:view_develop","checked":true}]"#,
        )
        .unwrap();
        assert_eq!(
            states[0],
            ItemState {
                id: "cmd:undo".into(),
                enabled: Some(false),
                checked: None
            }
        );
        assert_eq!(states[1].checked, Some(true));
        let node: MenuNode =
            serde_json::from_str(r#"{"kind":"item","id":"cmd:x","label":"X"}"#).unwrap();
        assert!(matches!(
            node,
            MenuNode::Item {
                enabled: true,
                checked: None,
                accelerator: None,
                ..
            }
        ));
        assert_eq!(command_of("cmd:rate_3"), Some("rate_3"));
        assert_eq!(command_of("rawtonic-quit"), None);
    }
}
