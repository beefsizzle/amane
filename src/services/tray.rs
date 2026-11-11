mod icon;
mod item;
mod menu;
mod watcher;

use crate::{Argument, Service};

pub use item::{TrayItem, TrayStatus};
pub use menu::{MenuEntry, MenuKind, TrayMenu};

#[derive(Default)]
pub struct Tray {
    items: Vec<TrayItem>,

    menu: Option<TrayMenu>,

    running: bool,
}

/*
 * amane is the tray here: programs register their icons with it,
 * and it reads each one again when the program says it changed
 */
impl Service for Tray {
    fn new() -> Self {
        Self::default()
    }

    fn listen() {
        watcher::run();
    }
}

impl Tray {
    // in the order the programs registered
    pub fn items(&self) -> &[TrayItem] {
        &self.items
    }

    // false when another tray, like waybar's, already owns the name
    pub fn running(&self) -> bool {
        self.running
    }

    // the menu open_menu asked for, once the program has answered
    pub fn menu(&self) -> Option<&TrayMenu> {
        self.menu.as_ref()
    }

    // a left click
    pub fn activate(id: u32) {
        item::call(id, "Activate", at_pointer());
    }

    // a middle click
    pub fn secondary_activate(id: u32) {
        item::call(id, "SecondaryActivate", at_pointer());
    }

    // asks the program to show a menu of its own, for an item that has none to read
    pub fn context_menu(id: u32) {
        item::call(id, "ContextMenu", at_pointer());
    }

    // the wheel over the icon, delta being the steps turned
    pub fn scroll(id: u32, delta: i32, horizontal: bool) {
        let direction = if horizontal { "horizontal" } else { "vertical" };

        let arguments = vec![Argument::from(delta), Argument::from(direction)];

        item::call(id, "Scroll", arguments);
    }

    // reads the item's menu, which menu() gives once the program has answered
    pub fn open_menu(id: u32) {
        menu::open(id);
    }

    // reads the menu again as a submenu opens, since some programs only fill it in then
    pub fn open_submenu(entry: i32) {
        menu::open_submenu(entry);
    }

    // presses an entry of the open menu
    pub fn click_menu(entry: i32) {
        menu::click(entry);
    }

    pub fn close_menu() {
        menu::close();
    }

    // new items go where their number puts them, so the order never shuffles
    fn show(&mut self, item: TrayItem) {
        let place = self.items.iter().position(|shown| shown.id >= item.id);

        match place {
            Some(index) if self.items[index].id == item.id => self.items[index] = item,
            Some(index) => self.items.insert(index, item),
            None => self.items.push(item),
        }
    }
}

// the calls take where on the screen the click was, which a wayland shell can't know
fn at_pointer() -> Vec<Argument> {
    vec![Argument::Int(0), Argument::Int(0)]
}
