use smithay_client_toolkit::seat::keyboard::{
    KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers,
};
use wayland_client::{
    Connection, QueueHandle,
    protocol::{wl_keyboard::WlKeyboard, wl_surface::WlSurface},
};

use crate::Key;
use crate::input::focus;

use super::WaylandState;

impl KeyboardHandler for WaylandState {
    // keys only say which keyboard they came from, so the focused window is kept here
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        surface: &WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
        self.keyboard_focus = Some(surface.clone());
    }

    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        surface: &WlSurface,
        _: u32,
    ) {
        self.keyboard_focus = None;

        // keys stop arriving, so no text input can stay focused
        focus::clear();

        self.request_frame_on(surface);
    }

    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.send_key(to_key(&event));
    }

    // for a compositor that repeats keys itself, the timer in seat.rs does it for the others
    fn repeat_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.repeat_held_key(&event);
    }

    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        _: u32,
        _: Modifiers,
        _: RawModifiers,
        _: u32,
    ) {
    }
}

impl WaylandState {
    fn send_key(&mut self, key: Key) {
        let Some(surface) = self.keyboard_focus.clone() else {
            return;
        };

        // a focused text input takes the key before the window's on_key sees it
        if focus::send(key) {
            self.request_frame_on(&surface);

            return;
        }

        let Some(window) = self.window(&surface) else {
            return;
        };

        let Some(on_key) = &window.on_key else {
            return;
        };

        on_key(key);

        // a service the handler changed wakes its own readers, this window redraws for the rest
        self.request_frame_on(&surface);
    }

    pub(super) fn repeat_held_key(&mut self, event: &KeyEvent) {
        let key = to_key(event);

        if repeats(key, focus::any()) {
            self.send_key(key);
        }
    }
}

/*
 * text keeps typing and the arrows keep moving, but enter, escape and a window's
 * letter shortcuts stay one press each, so holding one can't confirm or close twice
 */
fn repeats(key: Key, typing: bool) -> bool {
    match key {
        Key::Up | Key::Down | Key::Left | Key::Right => true,
        Key::Backspace | Key::Space | Key::Character(_) => typing,
        _ => false,
    }
}

fn to_key(event: &KeyEvent) -> Key {
    match event.keysym {
        Keysym::Return | Keysym::KP_Enter => Key::Enter,
        Keysym::Escape => Key::Escape,
        Keysym::Tab => Key::Tab,
        Keysym::BackSpace => Key::Backspace,
        Keysym::space => Key::Space,
        Keysym::Up => Key::Up,
        Keysym::Down => Key::Down,
        Keysym::Left => Key::Left,
        Keysym::Right => Key::Right,
        Keysym::Home => Key::Home,
        Keysym::End => Key::End,
        _ => character(event),
    }
}

// the text a key types already has shift and the keyboard layout applied
fn character(event: &KeyEvent) -> Key {
    let Some(text) = &event.utf8 else {
        return Key::Other;
    };

    let mut letters = text.chars();

    // a key that types more than one character has no single letter to give
    let (Some(letter), None) = (letters.next(), letters.next()) else {
        return Key::Other;
    };

    // with ctrl held, keys type invisible control characters
    if letter.is_control() {
        return Key::Other;
    }

    Key::Character(letter)
}

#[cfg(test)]
mod tests {
    use super::repeats;
    use crate::Key;

    #[test]
    fn typing_repeats_but_confirming_does_not() {
        for key in [
            Key::Character('a'),
            Key::Space,
            Key::Backspace,
            Key::Left,
            Key::Right,
        ] {
            assert!(repeats(key, true), "{key:?}");
        }

        for key in [Key::Enter, Key::Escape, Key::Tab, Key::Other] {
            assert!(!repeats(key, true), "{key:?}");
        }
    }

    #[test]
    fn a_window_only_gets_the_arrows_repeated() {
        for key in [Key::Up, Key::Down, Key::Left, Key::Right] {
            assert!(repeats(key, false), "{key:?}");
        }

        for key in [
            Key::Character('c'),
            Key::Space,
            Key::Backspace,
            Key::Enter,
            Key::Escape,
            Key::Home,
            Key::End,
        ] {
            assert!(!repeats(key, false), "{key:?}");
        }
    }
}
