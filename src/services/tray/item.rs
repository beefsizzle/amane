use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::thread;
use std::time::Duration;

use crate::{Argument, Bus, Service, Tray};

use super::icon;

pub const INTERFACE: &str = "org.kde.StatusNotifierItem";

// what a program without a menu may give as its menu's path
const NO_MENU: &str = "/NO_DBUSMENU";

// a program starting sends a few signals at once, and they make one read
const SETTLE: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, PartialEq)]
pub struct TrayItem {
    pub(crate) id: u32,

    pub(crate) name: String,

    pub(crate) title: String,

    pub(crate) status: TrayStatus,

    pub(crate) icon_path: Option<PathBuf>,

    pub(crate) is_menu: bool,

    // the program's connection, and where on it the item and its menu are
    pub(crate) owner: String,

    pub(crate) path: String,

    pub(crate) menu: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TrayStatus {
    // nothing to tell the user, most trays hide these
    Passive,

    // what an item without a status gets
    #[default]
    Active,

    NeedsAttention,
}

impl TrayItem {
    // pass this to the control functions
    pub fn id(&self) -> u32 {
        self.id
    }

    // the program's own name for the item, like "steam", the same every session
    pub fn name(&self) -> &str {
        &self.name
    }

    // what the item is, for a tooltip
    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn status(&self) -> TrayStatus {
        self.status
    }

    // the image file to draw, the attention icon while the item needs attention
    pub fn icon_path(&self) -> Option<&Path> {
        self.icon_path.as_deref()
    }

    // whether open_menu has a menu to read
    pub fn has_menu(&self) -> bool {
        !self.menu.is_empty()
    }

    // a left click has nothing to do but open the menu
    pub fn is_menu(&self) -> bool {
        self.is_menu
    }
}

impl TrayStatus {
    fn from_name(name: &str) -> Self {
        match name {
            "Passive" => Self::Passive,
            "NeedsAttention" => Self::NeedsAttention,
            _ => Self::Active,
        }
    }
}

/*
 * each item is read on a thread of its own, since a call to a program
 * that hangs never comes back, and that must not hold up the other icons;
 * every message is a change, and says whether the icon is one of them
 */
pub fn follow(id: u32, owner: &str, path: &str, changes: &Receiver<bool>, gone: &AtomicBool) {
    let mut shown: Option<TrayItem> = None;

    while let Ok(mut icon_changed) = changes.recv() {
        thread::sleep(SETTLE);

        while let Ok(more) = changes.try_recv() {
            icon_changed |= more;
        }

        let item = read(id, owner, path, shown.as_ref(), icon_changed);

        if shown.as_ref() == Some(&item) {
            continue;
        }

        let mut tray = Tray::write();

        // the program left while it was read, and the watcher already took it out
        if gone.load(Ordering::SeqCst) {
            tray.quiet();

            icon::remove(item.icon_path.as_deref());

            break;
        }

        tray.show(item.clone());

        drop(tray);

        // the icon it showed before is not drawn anymore
        let old_icon = shown.take().and_then(|before| before.icon_path);

        if old_icon != item.icon_path {
            icon::remove(old_icon.as_deref());
        }

        shown = Some(item);
    }

    icon::remove(shown.and_then(|item| item.icon_path).as_deref());
}

fn read(
    id: u32,
    owner: &str,
    path: &str,
    before: Option<&TrayItem>,
    icon_changed: bool,
) -> TrayItem {
    let bus = Bus::session();

    let property = |name: &str| bus.property(owner, path, INTERFACE, name);

    let status = TrayStatus::from_name(property("Status").text());

    // a saved icon stays until the program says it changed, or its status asks for another
    let kept = before
        .filter(|before| !icon_changed && before.status == status)
        .and_then(|before| before.icon_path.as_deref());

    let attention = status == TrayStatus::NeedsAttention;

    let menu = property("Menu");

    let menu = match menu.text() {
        NO_MENU => "",
        menu => menu,
    };

    TrayItem {
        id,

        name: String::from(property("Id").text()),
        title: String::from(property("Title").text()),
        status,
        icon_path: icon::find(&property, id, attention, kept),
        is_menu: property("ItemIsMenu").bool(),

        owner: String::from(owner),
        path: String::from(path),
        menu: String::from(menu),
    }
}

// the program may take a while to answer, so nothing waits for it
pub fn call(id: u32, method: &'static str, arguments: Vec<Argument>) {
    let address = Tray::read()
        .items
        .iter()
        .find(|item| item.id == id)
        .map(|item| (item.owner.clone(), item.path.clone()));

    let Some((owner, path)) = address else {
        return;
    };

    thread::spawn(move || {
        Bus::session().call(&owner, &path, INTERFACE, method, &arguments);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_item_without_a_status_is_active() {
        assert_eq!(TrayStatus::from_name("Passive"), TrayStatus::Passive);
        assert_eq!(
            TrayStatus::from_name("NeedsAttention"),
            TrayStatus::NeedsAttention
        );
        assert_eq!(TrayStatus::from_name("Active"), TrayStatus::Active);
        assert_eq!(TrayStatus::from_name(""), TrayStatus::Active);
    }
}
