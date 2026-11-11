use amane::{
    App, Button, Color, Column, Horizontal, Image, Layer, LayerWindow, MenuEntry, MenuKind,
    Padding, Parent, Rectangle, Row, Scroll, Service, Text, Tray, TrayItem, TrayStatus, Vertical,
    Widget,
};

const ICON: f32 = 24.0;

const ROW: f32 = 26.0;

/*
 * stop your tray first (waybar's, a quickshell bar), then start a program
 * with a tray icon, like nm-applet; left click an icon to open the program,
 * right click it for its menu
 */
fn main() {
    App::new().window(view).run();
}

fn view() -> LayerWindow {
    let tray = Tray::read();

    let mut icons: Vec<Box<dyn Widget>> = Vec::new();

    if !tray.running() {
        let warning = Text::new("another tray is running").size(16.0);

        icons.push(Box::new(warning.color(Color::WHITE)));
    }

    for item in tray.items() {
        // a passive item has nothing to tell, so a tray hides it
        if item.status() != TrayStatus::Passive {
            icons.push(Box::new(icon(item)));
        }
    }

    let mut rows: Vec<Box<dyn Widget>> = vec![Box::new(Row::new(icons).gap(4.0))];

    if let Some(menu) = tray.menu() {
        add_entries(&mut rows, menu.entries(), 0.0);
    }

    // as tall as what it shows, so its empty space doesn't block clicks on the windows underneath
    let height = ICON + ROW * (rows.len() - 1) as f32;

    LayerWindow::new()
        .width(300.0)
        .height(height)
        .anchor_vertical(Vertical::Top)
        .anchor_horizontal(Horizontal::Right)
        .layer(Layer::Top)
        .child(
            Rectangle::new()
                .width(Parent)
                .height(Parent)
                .fill(Color::BLACK)
                .child(Column::new(rows)),
        )
}

fn icon(item: &TrayItem) -> Rectangle {
    let id = item.id();

    let (has_menu, is_menu) = (item.has_menu(), item.is_menu());

    let square = Rectangle::new().width(ICON).height(ICON);

    let square = match item.icon_path() {
        Some(path) => square.fill(Image::contain(path)),
        None => square.fill(Color::BLUE),
    };

    square
        .on_click(move |button| clicked(id, button, has_menu, is_menu))
        .on_scroll(move |scroll| scrolled(id, scroll))
}

fn clicked(id: u32, button: Button, has_menu: bool, is_menu: bool) {
    match button {
        // an item that is only a menu has nothing else to do on a left click
        Button::Left if !is_menu => Tray::activate(id),

        Button::Middle => Tray::secondary_activate(id),

        _ if has_menu => toggle_menu(id),

        // without a menu to read, the program is asked to show its own
        _ => Tray::context_menu(id),
    }
}

// a second click on the same icon closes its menu
fn toggle_menu(id: u32) {
    let open = Tray::read().menu().is_some_and(|menu| menu.item() == id);

    if open {
        Tray::close_menu();
    } else {
        Tray::open_menu(id);
    }
}

fn scrolled(id: u32, scroll: Scroll) {
    let steps = if scroll.y < 0.0 { -1 } else { 1 };

    Tray::scroll(id, steps, false);
}

// a submenu's entries go under their parent, a step to the right
fn add_entries(rows: &mut Vec<Box<dyn Widget>>, entries: &[MenuEntry], indent: f32) {
    for entry in entries {
        rows.push(Box::new(entry_row(entry, indent)));

        add_entries(rows, entry.children(), indent + 16.0);
    }
}

fn entry_row(entry: &MenuEntry, indent: f32) -> Rectangle {
    let label = match entry.kind() {
        MenuKind::Separator => String::from("--------"),
        MenuKind::Check(true) | MenuKind::Radio(true) => format!("[x] {}", entry.label()),
        MenuKind::Check(false) | MenuKind::Radio(false) => format!("[ ] {}", entry.label()),
        MenuKind::Normal => String::from(entry.label()),
    };

    let color = if entry.enabled() {
        Color::WHITE
    } else {
        Color::rgb(128, 128, 128)
    };

    let padding = Padding {
        top: 0.0,
        right: 0.0,
        bottom: 0.0,
        left: indent,
    };

    let row = Rectangle::new()
        .width(Parent)
        .height(ROW)
        .padding(padding)
        .child(Text::new(label).size(16.0).color(color).elide());

    if !entry.enabled() || entry.kind() == MenuKind::Separator {
        return row;
    }

    let (id, has_submenu) = (entry.id(), entry.has_submenu());

    row.on_click(move |_| pressed(id, has_submenu))
}

fn pressed(entry: i32, has_submenu: bool) {
    // some programs only fill a submenu in as it opens
    if has_submenu {
        Tray::open_submenu(entry);

        return;
    }

    Tray::click_menu(entry);
    Tray::close_menu();
}
