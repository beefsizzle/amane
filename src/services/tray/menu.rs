use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{Argument, Bus, Service, Tray, Value};

const INTERFACE: &str = "com.canonical.dbusmenu";

// counts the menus asked for and closed, so an answer that comes late is dropped
static REQUEST: AtomicU64 = AtomicU64::new(0);

// the menu of one tray item, as the program described it when it was opened
#[derive(Debug, Clone, PartialEq)]
pub struct TrayMenu {
    pub(crate) item: u32,

    pub(crate) entries: Vec<MenuEntry>,

    // the program's connection, and where on it the menu is
    pub(crate) owner: String,

    pub(crate) path: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MenuEntry {
    pub(crate) id: i32,

    pub(crate) label: String,

    pub(crate) kind: MenuKind,

    pub(crate) enabled: bool,

    pub(crate) submenu: bool,

    pub(crate) children: Vec<MenuEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuKind {
    Normal,

    // a line between groups of entries, with no label
    Separator,

    // an entry with a tick, and whether it is on
    Check(bool),

    // one of a group where only one is on
    Radio(bool),
}

impl TrayMenu {
    // the id of the tray item it belongs to
    pub fn item(&self) -> u32 {
        self.item
    }

    // without the ones the program hides
    pub fn entries(&self) -> &[MenuEntry] {
        &self.entries
    }
}

impl MenuEntry {
    // pass this to Tray::click_menu, or to Tray::open_submenu
    pub fn id(&self) -> i32 {
        self.id
    }

    // without the underscore that marks a shortcut letter
    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn kind(&self) -> MenuKind {
        self.kind
    }

    // false for a greyed out entry, which does nothing when clicked
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    // true also while the submenu is still empty, see Tray::open_submenu
    pub fn has_submenu(&self) -> bool {
        self.submenu
    }

    pub fn children(&self) -> &[MenuEntry] {
        &self.children
    }
}

pub fn open(id: u32) {
    let request = REQUEST.fetch_add(1, Ordering::SeqCst) + 1;

    let address = Tray::read()
        .items
        .iter()
        .find(|item| item.id == id && item.has_menu())
        .map(|item| (item.owner.clone(), item.menu.clone()));

    let Some((owner, path)) = address else {
        return;
    };

    // the program can take a second to send its menu, so nothing waits for it
    thread::spawn(move || {
        let Some(entries) = read(&owner, &path, 0) else {
            return;
        };

        let mut tray = Tray::write();

        // another menu was asked for by now, or this one closed
        if REQUEST.load(Ordering::SeqCst) != request {
            tray.quiet();

            return;
        }

        tray.menu = Some(TrayMenu {
            item: id,
            entries,
            owner,
            path,
        });
    });
}

pub fn open_submenu(entry: i32) {
    let request = REQUEST.load(Ordering::SeqCst);

    let Some((owner, path)) = address() else {
        return;
    };

    thread::spawn(move || {
        let Some(entries) = read(&owner, &path, entry) else {
            return;
        };

        let mut tray = Tray::write();

        match tray.menu.as_mut() {
            Some(menu) if REQUEST.load(Ordering::SeqCst) == request => menu.entries = entries,
            _ => tray.quiet(),
        }
    });
}

// the menu is looked up first, so closing it right after still sends the click
pub fn click(entry: i32) {
    let Some((owner, path)) = address() else {
        return;
    };

    thread::spawn(move || {
        let now = SystemTime::now().duration_since(UNIX_EPOCH);

        let seconds = now.map_or(0, |now| now.as_secs() as u32);

        // the entry, what happened to it, a value clicks have no use for, and when
        let arguments = [
            Argument::from(entry),
            Argument::from("clicked"),
            Argument::Variant(Box::new(Argument::Int(0))),
            Argument::from(seconds),
        ];

        Bus::session().call(&owner, &path, INTERFACE, "Event", &arguments);
    });
}

pub fn close() {
    REQUEST.fetch_add(1, Ordering::SeqCst);

    let mut tray = Tray::write();

    if tray.menu.is_none() {
        tray.quiet();
    }

    tray.menu = None;
}

// where the open menu is
fn address() -> Option<(String, String)> {
    let tray = Tray::read();

    let menu = tray.menu.as_ref()?;

    Some((menu.owner.clone(), menu.path.clone()))
}

// the whole menu, after telling the program which part of it opens, 0 being the menu itself
fn read(owner: &str, path: &str, opening: i32) -> Option<Vec<MenuEntry>> {
    let bus = Bus::session();

    // some programs only fill a menu in when it is about to be shown
    bus.call(
        owner,
        path,
        INTERFACE,
        "AboutToShow",
        &[Argument::from(opening)],
    );

    // from the root, every level deep, with every property
    let arguments = [
        Argument::Int(0),
        Argument::Int(-1),
        Argument::TextList(Vec::new()),
    ];

    let answer = bus.call(owner, path, INTERFACE, "GetLayout", &arguments);

    // the answer is the layout's revision and its root, which only holds the entries
    let [_revision, root] = answer.list() else {
        return None;
    };

    Some(entries(root))
}

// each node is (id, properties, children)
fn entries(node: &Value) -> Vec<MenuEntry> {
    let [_, _, children] = node.list() else {
        return Vec::new();
    };

    children.list().iter().filter_map(entry).collect()
}

// none for an entry the program hides, or one with nothing to show
fn entry(node: &Value) -> Option<MenuEntry> {
    let [id, properties, _] = node.list() else {
        return None;
    };

    if properties.get("visible") == &Value::Bool(false) {
        return None;
    }

    let kind = kind(properties);

    let label = label(properties.get("label").text());

    if label.is_empty() && kind != MenuKind::Separator {
        return None;
    }

    let children = entries(node);

    Some(MenuEntry {
        id: id.number() as i32,

        label,
        kind,

        // an entry is enabled unless the program says it is not
        enabled: properties.get("enabled") != &Value::Bool(false),

        submenu: properties.get("children-display").text() == "submenu" || !children.is_empty(),
        children,
    })
}

fn kind(properties: &Value) -> MenuKind {
    if properties.get("type").text() == "separator" {
        return MenuKind::Separator;
    }

    let on = properties.get("toggle-state").number() == 1.0;

    match properties.get("toggle-type").text() {
        "checkmark" => MenuKind::Check(on),
        "radio" => MenuKind::Radio(on),
        _ => MenuKind::Normal,
    }
}

// one underscore marks the shortcut letter, two stand for an underscore
fn label(text: &str) -> String {
    text.replace("__", "\0").replace('_', "").replace('\0', "_")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn node(id: f64, properties: &[(&str, Value)], children: Vec<Value>) -> Value {
        let mut map = BTreeMap::new();

        for (name, value) in properties {
            map.insert(String::from(*name), value.clone());
        }

        Value::List(vec![
            Value::Number(id),
            Value::Map(map),
            Value::List(children),
        ])
    }

    fn text(text: &str) -> Value {
        Value::Text(String::from(text))
    }

    #[test]
    fn reads_a_menu_as_the_program_describes_it() {
        let quit = node(5.0, &[("label", text("_Quit"))], Vec::new());

        let root = node(
            0.0,
            &[],
            vec![
                node(1.0, &[("label", text("_Open my__app"))], Vec::new()),
                node(
                    2.0,
                    &[
                        ("label", text("Mute")),
                        ("toggle-type", text("checkmark")),
                        ("toggle-state", Value::Number(1.0)),
                        ("enabled", Value::Bool(false)),
                    ],
                    Vec::new(),
                ),
                node(3.0, &[("type", text("separator"))], Vec::new()),
                node(4.0, &[("label", text("More"))], vec![quit]),
                node(
                    6.0,
                    &[
                        ("label", text("Later")),
                        ("children-display", text("submenu")),
                    ],
                    Vec::new(),
                ),
                node(
                    7.0,
                    &[("label", text("Hidden")), ("visible", Value::Bool(false))],
                    Vec::new(),
                ),
                node(8.0, &[], Vec::new()),
            ],
        );

        let entries = entries(&root);

        let ids: Vec<i32> = entries.iter().map(MenuEntry::id).collect();

        assert_eq!(ids, [1, 2, 3, 4, 6]);

        assert_eq!(entries[0].label(), "Open my_app");
        assert_eq!(entries[0].kind(), MenuKind::Normal);
        assert!(entries[0].enabled());
        assert!(!entries[0].has_submenu());

        assert_eq!(entries[1].kind(), MenuKind::Check(true));
        assert!(!entries[1].enabled());

        assert_eq!(entries[2].kind(), MenuKind::Separator);

        assert!(entries[3].has_submenu());
        assert_eq!(entries[3].children()[0].label(), "Quit");

        // a submenu the program has not filled in yet
        assert!(entries[4].has_submenu());
        assert!(entries[4].children().is_empty());
    }

    #[test]
    fn an_answer_that_is_not_a_layout_is_no_menu() {
        assert!(entries(&Value::Nothing).is_empty());
        assert!(entry(&text("not a node")).is_none());
    }
}
