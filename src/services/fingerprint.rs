use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, Once, PoisonError};
use std::thread;
use std::time::Duration;

use zbus::blocking::Connection;
use zbus::zvariant::OwnedObjectPath;

use super::lock::Lock;
use crate::dbus::{Bus, Signal};

/*
 * unlocking with a fingerprint, through fprintd on the system bus. it runs next to the password: the reader waits for
 * a finger the whole time the session is locked, while the password field stays usable. pam_fprintd can't do that,
 * it blocks the PAM conversation until the finger comes or it times out, and the password has to wait behind it.
 *
 * only fprintd decides a finger matches. its answer arrives as a VerifyStatus signal, and any program on the system
 * bus can send a signal by that name, so one counts only when it comes from fprintd's own connection (its unique
 * name, which the bus fills in and nobody can fake) and from the device this lock claimed
 */
const NAME: &str = "net.reactivated.Fprint";
const MANAGER_PATH: &str = "/net/reactivated/Fprint/Manager";
const MANAGER: &str = "net.reactivated.Fprint.Manager";
const DEVICE: &str = "net.reactivated.Fprint.Device";

/// what the fingerprint reader is doing for the lock screen, see `Lock::use_fingerprint`
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Fingerprint {
    /// not turned on, or the session isn't locked
    #[default]
    Off,
    /// no reader, no enrolled finger, or the reader is in use or failed
    Unavailable,
    /// waiting for a finger
    Listening,
    /// the scan was poor (too short, off center), waiting for another
    Retry,
    /// a finger that isn't enrolled, waiting for another
    NoMatch,
}

static ENABLED: AtomicBool = AtomicBool::new(false);

// counts locks, so an answer meant for an earlier lock changes nothing
static ROUND: AtomicU64 = AtomicU64::new(0);

// a lock is on and wants the reader (between begin and end)
static ON: AtomicBool = AtomicBool::new(false);

// the reader this lock holds, and who may speak for it
struct Claim {
    round: u64,
    owner: String, // fprintd's unique name, like :1.83
    device: String,
}

static CLAIMED: Mutex<Option<Claim>> = Mutex::new(None);

// held while the reader is being claimed or given back, so two of those never interleave
static WORKING: Mutex<()> = Mutex::new(());

static WATCHING: Once = Once::new();

// how long a busy or broken reader is left before it's tried again
const RETRY_AFTER: Duration = Duration::from_secs(3);

// how long "not recognised" or "try again" stays up before it goes back to listening
const MESSAGE_FOR: Duration = Duration::from_secs(2);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(super) fn enable() {
    ENABLED.store(true, Ordering::Relaxed);
}

/// the round of the lock that's on now, for checking an answer still belongs to it
pub(super) fn current(round: u64) -> bool {
    ON.load(Ordering::SeqCst) && ROUND.load(Ordering::SeqCst) == round
}

// a lock started: claim the reader and wait for a finger, unless fingerprints are off
pub(super) fn begin() {
    let round = ROUND.fetch_add(1, Ordering::SeqCst) + 1;

    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }

    ON.store(true, Ordering::SeqCst);

    watch();

    // fprintd may have to start first, and claiming the reader is a few calls; none of it may hold up the lock
    thread::spawn(move || {
        let _working = lock(&WORKING);

        keep_trying(round, 0);
    });
}

// the lock ended: give the reader back
pub(super) fn end() {
    ON.store(false, Ordering::SeqCst);
    ROUND.fetch_add(1, Ordering::SeqCst);

    let Some(claim) = lock(&CLAIMED).take() else { return };

    thread::spawn(move || {
        let _working = lock(&WORKING);

        give_back(&claim);
    });
}

/*
 * claims the reader for the lock in `round` and starts listening. a reader that's busy (a sudo prompt holds it) or
 * broken is tried again every few seconds while the lock lasts. one that isn't there is tried `missing` more times:
 * none at the start of a lock (that machine has no reader, or no finger enrolled), a few after a restart, since a USB
 * reader can take a while to come back from sleep. call it holding WORKING
 */
fn keep_trying(round: u64, mut missing: u32) {
    while current(round) {
        match claim(round) {
            Ok(true) => {
                Lock::fingerprint_changed(round, Fingerprint::Listening);

                return;
            }
            Ok(false) if missing == 0 => {
                Lock::fingerprint_changed(round, Fingerprint::Unavailable);

                return;
            }
            Ok(false) => {
                missing -= 1;

                Lock::fingerprint_changed(round, Fingerprint::Unavailable);

                thread::sleep(RETRY_AFTER);
            }
            Err(_) => {
                Lock::fingerprint_changed(round, Fingerprint::Unavailable);

                thread::sleep(RETRY_AFTER);
            }
        }
    }
}

// gives the reader back and claims it again for the lock that's on: after a reader error, a wake from sleep (USB
// readers are often reset), or fprintd starting over
fn restart() {
    if !ON.load(Ordering::SeqCst) {
        return;
    }

    let round = ROUND.load(Ordering::SeqCst);

    thread::spawn(move || {
        let _working = lock(&WORKING);

        if let Some(claim) = lock(&CLAIMED).take() {
            give_back(&claim);
        }

        // a reader that just came back from sleep can take a moment, half a minute at most
        thread::sleep(Duration::from_millis(500));

        keep_trying(round, 10);
    });
}

fn give_back(claim: &Claim) {
    let _ = call(&claim.device, DEVICE, "VerifyStop");
    let _ = call(&claim.device, DEVICE, "Release");
}

fn system() -> zbus::Result<&'static Connection> {
    Bus::system().connection.ok_or(zbus::Error::Unsupported)
}

// a method of fprintd's that takes no arguments, or one string
fn call(path: &str, interface: &str, method: &str) -> zbus::Result<zbus::message::Message> {
    system()?.call_method(Some(NAME), path, Some(interface), method, &())
}

fn call_with(path: &str, interface: &str, method: &str, argument: &str) -> zbus::Result<zbus::message::Message> {
    system()?.call_method(Some(NAME), path, Some(interface), method, &(argument,))
}

// the reader, or None when there's nothing to listen with: no fprintd, no reader, or no finger enrolled for this
// user. those don't change while the screen is locked, so they aren't tried again
fn reader() -> Option<(String, String)> {
    // asking for the reader also starts fprintd, which runs on demand
    let device: OwnedObjectPath = call(MANAGER_PATH, MANAGER, "GetDefaultDevice").ok()?.body().deserialize().ok()?;
    let device = device.to_string();

    // "" is the user this program runs as. fprintd answers an error, not an empty list, when none are enrolled
    let fingers: Vec<String> = call_with(&device, DEVICE, "ListEnrolledFingers", "").ok()?.body().deserialize().ok()?;

    if fingers.is_empty() {
        return None;
    }

    let owner: String = system()
        .ok()?
        .call_method(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", Some("org.freedesktop.DBus"), "GetNameOwner", &(NAME,))
        .ok()?
        .body()
        .deserialize()
        .ok()?;

    Some((owner, device))
}

// false when there's nothing to listen with, an error when the reader is busy or failed and is worth another try
fn claim(round: u64) -> zbus::Result<bool> {
    let Some((owner, device)) = reader() else { return Ok(false) };

    // the claim belongs to this connection, and ends with it
    call_with(&device, DEVICE, "Claim", "")?;

    {
        let mut claimed = lock(&CLAIMED);

        // the lock ended while this was on its way
        if !current(round) {
            drop(claimed);

            let _ = call(&device, DEVICE, "Release");

            return Ok(true);
        }

        *claimed = Some(Claim { round, owner, device: device.clone() });
    }

    if let Err(error) = call_with(&device, DEVICE, "VerifyStart", "any") {
        if let Some(claim) = lock(&CLAIMED).take() {
            give_back(&claim);
        }

        return Err(error);
    }

    Ok(true)
}

/*
 * threads for the program's whole life, made at the first lock: fprintd's answers, waking from sleep, and fprintd
 * starting over. each watches again if its bus connection ever drops out, rather than going quiet for good
 */
fn watch() {
    WATCHING.call_once(|| {
        // made here, before the first verify starts, so its answer can't slip past
        let first = Bus::system().signals(DEVICE, "VerifyStatus");

        thread::spawn(move || {
            let mut signals = Some(first);

            loop {
                for signal in signals.take().unwrap_or_else(|| Bus::system().signals(DEVICE, "VerifyStatus")) {
                    verified(&signal);
                }

                thread::sleep(RETRY_AFTER);
            }
        });

        thread::spawn(|| {
            loop {
                for signal in Bus::system().signals("org.freedesktop.login1.Manager", "PrepareForSleep") {
                    let going_to_sleep = signal.arguments().first().is_some_and(crate::Value::bool);

                    if !going_to_sleep {
                        restart();
                    }
                }

                thread::sleep(RETRY_AFTER);
            }
        });

        thread::spawn(|| {
            loop {
                for signal in Bus::system().signals("org.freedesktop.DBus", "NameOwnerChanged") {
                    owner_changed(&signal);
                }

                thread::sleep(RETRY_AFTER);
            }
        });
    });
}

// NameOwnerChanged(name, old owner, new owner), from the bus itself
fn owner_changed(signal: &Signal) {
    let arguments = signal.arguments();

    if signal.sender() != "org.freedesktop.DBus" || arguments.first().map_or("", crate::Value::text) != NAME {
        return;
    }

    let new_owner = arguments.get(2).map_or("", crate::Value::text);

    let ours = lock(&CLAIMED).as_ref().is_some_and(|claim| claim.owner != new_owner);

    // the fprintd that held our claim is gone, and the claim with it
    if ours {
        lock(&CLAIMED).take();

        restart();
    }
}

// VerifyStatus(result, done)
fn verified(signal: &Signal) {
    let (round, device) = {
        let claimed = lock(&CLAIMED);

        let Some(claim) = claimed.as_ref() else { return };

        // anyone can send a signal with this name: only fprintd, about our reader, counts
        if signal.sender() != claim.owner || signal.path() != claim.device {
            return;
        }

        (claim.round, claim.device.clone())
    };

    let result = signal.arguments().first().map_or("", crate::Value::text);

    match result {
        "verify-match" => Lock::fingerprint_matched(round),

        // fprintd stops after an answer, so it's asked to listen again
        "verify-no-match" => {
            Lock::fingerprint_changed(round, Fingerprint::NoMatch);

            thread::spawn(move || {
                let _ = call(&device, DEVICE, "VerifyStop");

                if current(round) {
                    let _ = call_with(&device, DEVICE, "VerifyStart", "any");
                }

                settle(round, Fingerprint::NoMatch);
            });
        }

        // a poor scan, fprintd keeps listening
        "verify-retry-scan" | "verify-swipe-too-short" | "verify-finger-not-centered" | "verify-remove-and-retry" => {
            Lock::fingerprint_changed(round, Fingerprint::Retry);

            thread::spawn(move || settle(round, Fingerprint::Retry));
        }

        // the reader went away or broke: the password still works, and the reader is tried again
        _ => {
            Lock::fingerprint_changed(round, Fingerprint::Unavailable);

            restart();
        }
    }
}

// a message about the last scan goes back to "listening" after a moment, unless something newer replaced it
fn settle(round: u64, shown: Fingerprint) {
    thread::sleep(MESSAGE_FOR);

    Lock::fingerprint_settled(round, shown);
}
