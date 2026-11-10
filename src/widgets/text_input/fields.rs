use std::any::TypeId;
use std::cell::RefCell;
use std::collections::HashMap;

use super::field::Field;
use crate::changes;

thread_local! {
    // what each text input holds, kept apart from the widgets that are rebuilt each redraw
    static FIELDS: RefCell<HashMap<&'static str, Field>> = RefCell::new(HashMap::new());
}

/*
 * read like a service, so a change draws again every window that shows an input, not only the one
 * typed in: a lock screen on each monitor shows the same password field
 */
struct Fields;

pub fn get(id: &'static str) -> Field {
    FIELDS.with_borrow(|fields| fields.get(id).cloned().unwrap_or_default())
}

// for drawing, so the window is drawn again when an input changes
pub fn read(id: &'static str) -> Field {
    changes::note_read(TypeId::of::<Fields>());

    get(id)
}

pub fn set(id: &'static str, field: Field) {
    let changed = FIELDS.with_borrow_mut(|fields| {
        if fields.get(id) == Some(&field) {
            return false;
        }

        fields.insert(id, field);

        true
    });

    // emptying an input that's already empty draws nothing again
    if changed {
        changes::mark(TypeId::of::<Fields>());
    }
}
