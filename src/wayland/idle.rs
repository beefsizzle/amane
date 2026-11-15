use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use wayland_client::{Connection, Dispatch, QueueHandle, protocol::wl_seat::WlSeat};
use wayland_protocols::ext::idle_notify::v1::client::{
    ext_idle_notification_v1::{self, ExtIdleNotificationV1},
    ext_idle_notifier_v1::{self, ExtIdleNotifierV1},
};

use crate::changes;

use super::WaylandState;

// set from any thread by Idle::hold and Idle::release, and followed on the event loop
static HELD: AtomicBool = AtomicBool::new(false);
static HELD_CHANGED: AtomicBool = AtomicBool::new(false);

// one App::idle: how long without input, and what runs when it passes and when input comes back
#[derive(Clone, Copy)]
pub struct IdleTimeout {
    pub after: Duration,
    pub idle: fn(),
    pub resumed: fn(),
}

// one timeout on one seat, and whether its idle ran without its resumed yet
pub(crate) struct IdleWatch {
    seat: WlSeat,
    index: usize,
    notification: ExtIdleNotificationV1,
    idled: bool,
}

pub struct Idle;

impl Idle {
    /*
     * holds every App::idle timeout off, for something the compositor can't
     * see as input, like a game played with a controller. a timeout that
     * already ran still gets its resumed when input comes back
     */
    pub fn hold() {
        set_held(true);
    }

    // the timeouts count again from now
    pub fn release() {
        set_held(false);
    }
}

fn set_held(held: bool) {
    HELD.store(held, Ordering::Relaxed);
    HELD_CHANGED.store(true, Ordering::Relaxed);

    changes::ping();
}

impl WaylandState {
    /*
     * asks the compositor for every timeout on this seat that it isn't watched for yet.
     * the compositor holds them off while something inhibits idle, like a playing video.
     * none while held, or when the compositor lacks ext-idle-notify
     */
    pub fn watch_idle(&mut self, seat: &WlSeat) {
        let Some(notifier) = &self.idle_notifier else {
            return;
        };

        if HELD.load(Ordering::Relaxed) {
            return;
        }

        for (index, timeout) in self.idle_timeouts.iter().enumerate() {
            let watched = self
                .idle_watches
                .iter()
                .any(|watch| &watch.seat == seat && watch.index == index);

            if watched {
                continue;
            }

            let notification = notifier.get_idle_notification(
                milliseconds(timeout.after),
                seat,
                &self.qh,
                *timeout,
            );

            self.idle_watches.push(IdleWatch {
                seat: seat.clone(),
                index,
                notification,
                idled: false,
            });
        }
    }

    pub fn unwatch_idle(&mut self, seat: &WlSeat) {
        self.idle_watches.retain(|watch| {
            if &watch.seat != seat {
                return true;
            }

            watch.notification.destroy();

            false
        });
    }

    // a fresh notification counts from when it's made, which is how a release starts over
    pub fn follow_idle_hold(&mut self) {
        if !HELD_CHANGED.swap(false, Ordering::Relaxed) {
            return;
        }

        if !HELD.load(Ordering::Relaxed) {
            let seats: Vec<WlSeat> = self.seat.seats().collect();

            for seat in &seats {
                self.watch_idle(seat);
            }

            return;
        }

        // one that already ran is kept until input comes back, so its resumed still runs
        self.idle_watches.retain(|watch| {
            if watch.idled {
                return true;
            }

            watch.notification.destroy();

            false
        });
    }
}

// the protocol counts in milliseconds, a longer timeout than it can say waits as long as it can
fn milliseconds(duration: Duration) -> u32 {
    u32::try_from(duration.as_millis()).unwrap_or(u32::MAX)
}

impl Dispatch<ExtIdleNotificationV1, IdleTimeout> for WaylandState {
    fn event(
        state: &mut Self,
        notification: &ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        timeout: &IdleTimeout,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(place) = state
            .idle_watches
            .iter()
            .position(|watch| &watch.notification == notification)
        else {
            return;
        };

        match event {
            ext_idle_notification_v1::Event::Idled => {
                // held since the compositor sent this, and about to be taken down
                if HELD.load(Ordering::Relaxed) {
                    return;
                }

                state.idle_watches[place].idled = true;

                (timeout.idle)();
            }
            ext_idle_notification_v1::Event::Resumed => {
                state.idle_watches[place].idled = false;

                (timeout.resumed)();

                // held while it was idle, so it goes now that it has resumed
                if HELD.load(Ordering::Relaxed) {
                    state.idle_watches.swap_remove(place).notification.destroy();
                }
            }
            _ => {}
        }
    }
}

// the notifier sends no events
impl Dispatch<ExtIdleNotifierV1, ()> for WaylandState {
    fn event(
        _: &mut Self,
        _: &ExtIdleNotifierV1,
        _: ext_idle_notifier_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeouts_fit_the_protocol() {
        assert_eq!(milliseconds(Duration::from_secs(300)), 300_000);
        assert_eq!(milliseconds(Duration::from_secs(u64::MAX)), u32::MAX);
    }
}
