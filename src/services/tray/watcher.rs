use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread;

use crate::{Argument, Bus, Method, Service, Signal, Tray, Value};

use super::{icon, item};

const NAME: &str = "org.kde.StatusNotifierWatcher";

const PATH: &str = "/StatusNotifierWatcher";

const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

// the bus itself, which knows every connection and name
const DBUS: &str = "org.freedesktop.DBus";

const DBUS_PATH: &str = "/org/freedesktop/DBus";

// where an item is when the program only gives a name
const ITEM_PATH: &str = "/StatusNotifierItem";

// what an item sends when something amane shows changed, and whether that is its icon
const SIGNALS: [(&str, bool); 4] = [
    ("NewIcon", true),
    ("NewAttentionIcon", true),
    ("NewStatus", false),
    ("NewTitle", false),
];

const WATCHER_PROPERTIES: [&str; 3] = [
    "IsStatusNotifierHostRegistered",
    "ProtocolVersion",
    "RegisteredStatusNotifierItems",
];

enum Event {
    // a program asked for an icon: what it said the item is, and its own connection
    Registered {
        service: String,
        caller: String,
    },

    // an item on this connection and path changed, maybe its icon
    Changed {
        owner: String,
        path: String,
        icon: bool,
    },

    // a connection left the bus, and its items with it
    Left(String),
}

// an item being followed, and the ways to reach its thread
struct Source {
    owner: String,

    path: String,

    changes: Sender<bool>,

    gone: Arc<AtomicBool>,
}

pub fn run() {
    let bus = Bus::session();

    // watched before owning the name, so no early call is missed
    let registrations = bus.methods(PATH, NAME);
    let property_calls = bus.methods(PATH, PROPERTIES);

    if !bus.own(NAME) {
        return;
    }

    Tray::write().running = true;

    icon::sweep();

    let (events, received) = mpsc::channel();

    for (name, icon) in SIGNALS {
        let signals = bus.signals(item::INTERFACE, name);

        let events = events.clone();

        thread::spawn(move || follow_items(signals, icon, &events));
    }

    let connections = bus.signals(DBUS, "NameOwnerChanged");

    {
        let events = events.clone();

        thread::spawn(move || follow_connections(connections, &events));
    }

    thread::spawn(move || answer_properties(property_calls));

    thread::spawn(move || answer_registrations(registrations, &events));

    let mut sources = Vec::new();

    let mut next_id = 1;

    for event in received {
        match event {
            Event::Registered { service, caller } => {
                if register(&mut sources, next_id, &service, &caller) {
                    next_id += 1;
                }
            }

            Event::Changed { owner, path, icon } => {
                let source = sources
                    .iter()
                    .find(|source| source.owner == owner && source.path == path);

                if let Some(source) = source {
                    let _ = source.changes.send(icon);
                }
            }

            Event::Left(connection) => remove(&mut sources, &connection),
        }
    }
}

fn register(sources: &mut Vec<Source>, id: u32, service: &str, caller: &str) -> bool {
    let (destination, path) = address(service, caller);

    // the same for every name a program has, and empty when it already left
    let owner = owner(&destination);

    let known = sources
        .iter()
        .any(|source| source.owner == owner && source.path == path);

    if owner.is_empty() || known {
        return false;
    }

    let (changes, receiver) = mpsc::channel();

    let gone = Arc::new(AtomicBool::new(false));

    {
        let (owner, path, gone) = (owner.clone(), path.clone(), gone.clone());

        thread::spawn(move || item::follow(id, &owner, &path, &receiver, &gone));
    }

    // the first read
    let _ = changes.send(true);

    announce("StatusNotifierItemRegistered", &owner, &path);

    sources.push(Source {
        owner,
        path,
        changes,
        gone,
    });

    true
}

fn remove(sources: &mut Vec<Source>, connection: &str) {
    let before = sources.len();

    sources.retain(|source| {
        if source.owner != connection {
            return true;
        }

        // set before the item is taken out, so its thread doesn't put it back
        source.gone.store(true, Ordering::SeqCst);

        announce(
            "StatusNotifierItemUnregistered",
            &source.owner,
            &source.path,
        );

        false
    });

    // most connections that leave had no item, so nothing is redrawn
    if sources.len() == before {
        return;
    }

    let mut tray = Tray::write();

    tray.items.retain(|item| item.owner != connection);

    if tray
        .menu
        .as_ref()
        .is_some_and(|menu| menu.owner == connection)
    {
        tray.menu = None;
    }
}

/*
 * where a registered item is: "name", "name/path", or only "/path" on
 * the caller's own connection, which programs without a name of their
 * own send, like steam
 */
fn address(service: &str, caller: &str) -> (String, String) {
    if service.starts_with('/') {
        return (String::from(caller), String::from(service));
    }

    match service.split_once('/') {
        Some((name, path)) => (String::from(name), format!("/{path}")),
        None => (String::from(service), String::from(ITEM_PATH)),
    }
}

// the connection behind a name, like ":1.42", which is what signals come from
fn owner(name: &str) -> String {
    let owner = Bus::session().call(DBUS, DBUS_PATH, DBUS, "GetNameOwner", &[name.into()]);

    String::from(owner.text())
}

// for other trays that follow the watcher's list
fn announce(signal: &str, owner: &str, path: &str) {
    let item = Argument::from(format!("{owner}{path}"));

    Bus::session().emit(PATH, NAME, signal, &[item]);
}

fn answer_registrations(registrations: impl Iterator<Item = Method>, events: &Sender<Event>) {
    for method in registrations {
        if method.name() == "RegisterStatusNotifierItem" {
            let service = method.arguments().first().map_or("", Value::text);

            let event = Event::Registered {
                service: String::from(service),
                caller: String::from(method.sender()),
            };

            let _ = events.send(event);
        }

        // every call waits for an answer, also the ones amane has no use for
        method.reply(&[]);
    }
}

fn answer_properties(calls: impl Iterator<Item = Method>) {
    for method in calls {
        match method.name() {
            "Get" => {
                let name = method.arguments().get(1).map_or("", Value::text);

                match property(name) {
                    Some(value) => method.reply(&[Argument::Variant(Box::new(value))]),
                    None => method.reply(&[]),
                }
            }

            "GetAll" => {
                let mut properties = BTreeMap::new();

                for name in WATCHER_PROPERTIES {
                    if let Some(value) = property(name) {
                        properties.insert(String::from(name), value);
                    }
                }

                method.reply(&[Argument::Map(properties)]);
            }

            _ => method.reply(&[]),
        }
    }
}

fn property(name: &str) -> Option<Argument> {
    match name {
        // programs ask this before they register, and some show no icon without a host
        "IsStatusNotifierHostRegistered" => Some(Argument::from(true)),

        "ProtocolVersion" => Some(Argument::Int(0)),

        "RegisteredStatusNotifierItems" => {
            let tray = Tray::read();

            let mut items = Vec::new();

            for item in tray.items() {
                items.push(format!("{}{}", item.owner, item.path));
            }

            Some(Argument::from(items))
        }

        _ => None,
    }
}

fn follow_items(signals: impl Iterator<Item = Signal>, icon: bool, events: &Sender<Event>) {
    for signal in signals {
        let event = Event::Changed {
            owner: String::from(signal.sender()),
            path: String::from(signal.path()),
            icon,
        };

        if events.send(event).is_err() {
            return;
        }
    }
}

// the arguments are the name, its old owner and its new one, which is empty when the name is gone
fn follow_connections(signals: impl Iterator<Item = Signal>, events: &Sender<Event>) {
    for signal in signals {
        let [name, _, new_owner] = signal.arguments() else {
            continue;
        };

        // only a connection's own name, like ":1.42", is what items are kept under
        if !name.text().starts_with(':') || !new_owner.text().is_empty() {
            continue;
        }

        if events.send(Event::Left(String::from(name.text()))).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_an_item_from_every_way_to_register() {
        let item = |service| address(service, ":1.7");

        assert_eq!(
            item("org.kde.StatusNotifierItem-5-1"),
            (
                String::from("org.kde.StatusNotifierItem-5-1"),
                String::from("/StatusNotifierItem")
            )
        );

        assert_eq!(
            item(":1.9/org/ayatana/NotificationItem/app"),
            (
                String::from(":1.9"),
                String::from("/org/ayatana/NotificationItem/app")
            )
        );

        assert_eq!(
            item("/org/ayatana/NotificationItem/steam"),
            (
                String::from(":1.7"),
                String::from("/org/ayatana/NotificationItem/steam")
            )
        );
    }
}
