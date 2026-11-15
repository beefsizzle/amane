# Idle

Amane can tell your shell when you've stepped away, so it can lock the session or turn the screens off, without a separate idle daemon.

Your compositor must support the **ext-idle-notify** protocol. niri, Hyprland, and Sway do. Without it, the timeouts never run.

## Timeouts

`App::idle` takes how long to wait without input, what to run when that time passes, and what to run when input comes back:

```rust
use std::time::Duration;

use amane::{App, Lock};

fn main() {
    App::new()
        .lock(lock_screen)
        .idle(Duration::from_secs(300), Lock::start, || {})
        .idle(Duration::from_secs(600), screens_off, screens_on)
        .run();
}
```

Each call adds one more timeout, and they all count from your last input. Here, the session locks after five minutes, and the screens go off after ten.

While something inhibits idle, like a video that's playing, the compositor holds every timeout off.

Both functions run on the event loop, so they should be quick. `Lock::start` is. For something slow, like waiting on a command, start a thread.

If you only need one side, pass `|| {}` for the other.

## Holding the timeouts off

Some input never reaches the compositor. A game played with a controller reads it straight from the device, so the session can lock in the middle of a match. Games ask the desktop to stay awake over D-Bus instead, through `org.freedesktop.ScreenSaver` or the desktop portal.

If your shell answers those requests, it can hold every timeout off while one is active:

```rust
use amane::Idle;

Idle::hold();

// later, once nothing asks anymore
Idle::release();
```

After `Idle::release`, the timeouts count again from that moment, so the session still locks five minutes after the game closes. A timeout that already ran when you call `Idle::hold`, like screens that are already off, still gets its `resumed` when input comes back.
