# System Tray

`Tray` makes your shell the system tray. Programs like Steam, Discord, and nm-applet hand their icons to Amane, the same way they'd hand them to waybar's tray, and you decide how to show them. It speaks StatusNotifierItem over D-Bus, the protocol current tray programs use.

It doesn't poll. A program announces when its icon changes, and only that icon is read again.

## Before you start

Only one tray can run at a time. Stop the one you have (waybar's tray, or a tray feature in another bar) before starting your shell. If another tray is running, `running()` returns `false` and Amane receives nothing.

Amane only becomes the tray once your shell reads `Tray` for the first time. Read it somewhere that runs at startup, like your bar. Programs that are already running notice the new tray and register with it by themselves.

## A row of icons

```rust
use amane::{
    App, Button, Color, Horizontal, Image, Layer, LayerWindow, Rectangle, Row, Service, Tray,
    TrayItem, TrayStatus, Vertical, Widget,
};

fn main() {
    App::new().window(view).run();
}

fn view() -> LayerWindow {
    let tray = Tray::read();

    let mut icons: Vec<Box<dyn Widget>> = Vec::new();

    for item in tray.items() {
        // a passive item has nothing to tell, so a tray hides it
        if item.status() != TrayStatus::Passive {
            icons.push(Box::new(icon(item)));
        }
    }

    LayerWindow::new()
        .width(300.0)
        .height(24.0)
        .anchor_vertical(Vertical::Top)
        .anchor_horizontal(Horizontal::Right)
        .layer(Layer::Top)
        .child(Row::new(icons).gap(4.0))
}

fn icon(item: &TrayItem) -> Rectangle {
    let id = item.id();

    let square = Rectangle::new().width(24.0).height(24.0);

    let square = match item.icon_path() {
        Some(path) => square.fill(Image::contain(path)),
        None => square.fill(Color::BLUE),
    };

    square.on_click(move |button| match button {
        Button::Left => Tray::activate(id),
        Button::Middle => Tray::secondary_activate(id),
        Button::Right => Tray::open_menu(id),
    })
}
```

Start a program with a tray icon, like `nm-applet`, and its icon shows up.

## Reading items

`items()` gives every icon, in the order the programs registered. Each `TrayItem` has:

| Function | Gives |
|---|---|
| `id()` | its number, which the control functions take |
| `name()` | the program's own name for the item, like `"steam"`, the same every session |
| `title()` | what the item is, to show as a tooltip. It may be empty. |
| `status()` | `TrayStatus::Passive`, `Active`, or `NeedsAttention` |
| `icon_path()` | the image file to draw, or `None`. While the item needs attention, it's the attention icon, when the program has one. |
| `has_menu()` | whether `open_menu` has a menu to read |
| `is_menu()` | `true` when a left click has nothing to do but open the menu |

`Passive` items have nothing to tell the user, and most trays hide them. `items()` still lists them, so that choice is yours.

A program that quits is removed from `items()` by itself.

## Where icons come from

`icon_path()` is a file you can draw like any other image ([Images](images.md)). Amane finds it in one of two ways:

- For a program that names its icon, it looks in the program's own folder, then in your icon theme.
- For a program that sends the picture itself, like Discord, it saves the picture as a PNG in `$XDG_RUNTIME_DIR/amane-tray`. Those files are removed when the picture changes and when the program quits.

Overlay icons and animated icons aren't read. Tooltips aren't either, so show `title()` instead.

Old programs that only have an XEmbed icon don't show up.

## Clicking and scrolling

These take the item's `id()`:

| Function | Does |
|---|---|
| `Tray::activate(id)` | what a left click does, usually opening the program's window. When `is_menu()` is `true`, open the menu instead. |
| `Tray::secondary_activate(id)` | what a middle click does |
| `Tray::scroll(id, delta, horizontal)` | the wheel over the icon, where `delta` is the number of steps |
| `Tray::context_menu(id)` | asks the program to show a menu of its own. Use it when `has_menu()` is `false`, though many programs can't place a menu on Wayland. |

All of them run on a background thread and return right away.

What each one does is up to the program. Some do nothing on a left click, and programs don't agree on which way a positive `delta` scrolls.

## Menus

Most items have a menu, and Amane hands it to you as data, so you draw it however you like. A menu is read when you ask for it, because many programs only fill theirs in then:

```rust,ignore
Tray::open_menu(item.id());
```

The program can take a moment to answer. When it has, `menu()` gives the menu, and the window draws again:

```rust,ignore
if let Some(menu) = tray.menu() {
    for entry in menu.entries() {
        rows.push(Box::new(row(entry)));
    }
}
```

One menu is open at a time. `menu.item()` is the `id()` of the tray item it belongs to. Each `MenuEntry` has:

| Function | Gives |
|---|---|
| `id()` | its number, which `click_menu` and `open_submenu` take |
| `label()` | the text to show, without the underscore that marks a shortcut letter |
| `kind()` | `MenuKind::Normal`, `Separator`, `Check(on)`, or `Radio(on)` |
| `enabled()` | `false` for a greyed out entry, which does nothing when clicked |
| `has_submenu()` | whether it opens a submenu |
| `children()` | the entries of its submenu |

Entries the program hides are left out.

| Function | Does |
|---|---|
| `Tray::open_menu(id)` | reads a tray item's menu, replacing the one that was open |
| `Tray::click_menu(entry)` | presses an entry of the open menu. The program does the rest. |
| `Tray::open_submenu(entry)` | reads the menu again as a submenu opens. Some programs only fill a submenu in then, so `children()` can be empty before it. |
| `Tray::close_menu()` | lets go of the menu, so `menu()` gives `None` again |

A menu stays as it was when it was opened. To show a change the program made since, like a tick that flipped, call `open_menu` again.

A click on an entry usually ends with closing the menu:

```rust,ignore
let id = entry.id();

row.on_click(move |_| {
    Tray::click_menu(id);
    Tray::close_menu();
})
```
