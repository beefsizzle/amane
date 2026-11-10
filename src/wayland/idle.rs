use std::time::Duration;

use wayland_client::{Connection, Dispatch, QueueHandle, protocol::wl_seat::WlSeat};
use wayland_protocols::ext::idle_notify::v1::client::{
    ext_idle_notification_v1::{self, ExtIdleNotificationV1},
    ext_idle_notifier_v1::{self, ExtIdleNotifierV1},
};

use super::WaylandState;

// one App::idle: how long without input, and what runs when it passes and when input comes back
#[derive(Clone, Copy)]
pub struct IdleTimeout {
    pub after: Duration,
    pub idle: fn(),
    pub resumed: fn(),
}

impl WaylandState {
    /*
     * asks the compositor for every timeout on this seat. the compositor holds them off while
     * something inhibits idle, like a playing video. none when it lacks ext-idle-notify
     */
    pub fn watch_idle(&mut self, seat: &WlSeat) {
        let Some(notifier) = &self.idle_notifier else {
            return;
        };

        for timeout in &self.idle_timeouts {
            let notification = notifier.get_idle_notification(
                milliseconds(timeout.after),
                seat,
                &self.qh,
                *timeout,
            );

            self.idle_notifications.push((seat.clone(), notification));
        }
    }

    pub fn unwatch_idle(&mut self, seat: &WlSeat) {
        self.idle_notifications.retain(|(watched, notification)| {
            if watched != seat {
                return true;
            }

            notification.destroy();

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
        _: &mut Self,
        _: &ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        timeout: &IdleTimeout,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_idle_notification_v1::Event::Idled => (timeout.idle)(),
            ext_idle_notification_v1::Event::Resumed => (timeout.resumed)(),
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
