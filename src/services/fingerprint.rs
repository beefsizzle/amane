use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, Once, PoisonError};
use std::thread;
use std::time::Duration;

use zbus::blocking::Connection;
use zbus::zvariant::OwnedObjectPath;

use super::lock::Lock;
use crate::dbus::{Bus, Signal};

/*
 * unlocking with a fingerprint through fprintd, next to the password. pam_fprintd can't run alongside a password
 * prompt, it holds the conversation until a finger comes or it times out
 */
const NAME: &str = "net.reactivated.Fprint";
const MANAGER_PATH: &str = "/net/reactivated/Fprint/Manager";
const MANAGER: &str = "net.reactivated.Fprint.Manager";
const DEVICE: &str = "net.reactivated.Fprint.Device";

// what the reader is doing for the lock screen
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Fingerprint {
    // not turned on, or the session isn't locked
    #[default]
    Off,

    // no reader, no enrolled finger, or the reader is busy or failed
    Unavailable,

    Listening,

    Retry,

    NoMatch,

    // given back until the next lock, the password still works
    TooManyTries,
}

static ENABLED: AtomicBool = AtomicBool::new(false);

// counts locks, so an answer meant for an earlier lock changes nothing
static ROUND: AtomicU64 = AtomicU64::new(0);

static ON: AtomicBool = AtomicBool::new(false);

// the reader this lock holds, and who may speak for it
struct Claim {
    round: u64,
    owner: String, // fprintd's unique name, like :1.83
    device: String,
}

static CLAIMED: Mutex<Option<Claim>> = Mutex::new(None);

// rejected fingers in this lock, changed only while holding CLAIMED
static MISSES: AtomicU32 = AtomicU32::new(0);

static WORKING: Mutex<()> = Mutex::new(());

static WATCHING: Once = Once::new();

// how long a busy or broken reader waits before another try
const RETRY_AFTER: Duration = Duration::from_secs(3);

// how long "not recognised" or "try again" stays up
const MESSAGE_FOR: Duration = Duration::from_secs(2);

// rejected fingers per lock, each one another draw against the matcher
const MAX_MISSES: u32 = 5;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(super) fn enable() {
    ENABLED.store(true, Ordering::Relaxed);
}

// whether an answer for `round` still belongs to the lock that's on
pub(super) fn current(round: u64) -> bool {
    ON.load(Ordering::SeqCst) && ROUND.load(Ordering::SeqCst) == round
}

fn spent() -> bool {
    MISSES.load(Ordering::SeqCst) >= MAX_MISSES
}

// claims the reader for a new lock, unless fingerprints are off
pub(super) fn begin() {
    let round = ROUND.fetch_add(1, Ordering::SeqCst) + 1;

    MISSES.store(0, Ordering::SeqCst);

    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }

    ON.store(true, Ordering::SeqCst);

    watch();

    // fprintd may have to start first, which mustn't hold up the lock
    thread::spawn(move || {
        let _working = lock(&WORKING);

        keep_trying(round, 0);
    });
}

pub(super) fn end() {
    ON.store(false, Ordering::SeqCst);
    ROUND.fetch_add(1, Ordering::SeqCst);

    let Some(claim) = lock(&CLAIMED).take() else {
        return;
    };

    thread::spawn(move || {
        let _working = lock(&WORKING);

        give_back(&claim);
    });
}

/*
 * claims the reader for `round` and starts listening. a busy or broken reader is tried again while the lock lasts,
 * a missing one only `missing` more times. call it holding WORKING
 */
fn keep_trying(round: u64, mut missing: u32) {
    while current(round) && !spent() {
        match claim(round) {
            Ok(true) => {
                if !spent() {
                    Lock::fingerprint_changed(round, Fingerprint::Listening);
                }

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

// claims the reader again after a reader error, a wake from sleep, or fprintd starting over
fn restart() {
    if !ON.load(Ordering::SeqCst) || spent() {
        return;
    }

    let round = ROUND.load(Ordering::SeqCst);

    thread::spawn(move || {
        let _working = lock(&WORKING);

        if let Some(claim) = lock(&CLAIMED).take() {
            give_back(&claim);
        }

        // a USB reader can take a while to come back from sleep
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

fn call(path: &str, interface: &str, method: &str) -> zbus::Result<zbus::message::Message> {
    system()?.call_method(Some(NAME), path, Some(interface), method, &())
}

fn call_with(
    path: &str,
    interface: &str,
    method: &str,
    argument: &str,
) -> zbus::Result<zbus::message::Message> {
    system()?.call_method(Some(NAME), path, Some(interface), method, &(argument,))
}

// the reader, or None without fprintd, a reader or an enrolled finger
fn reader() -> Option<(String, String)> {
    let device: OwnedObjectPath = call(MANAGER_PATH, MANAGER, "GetDefaultDevice")
        .ok()?
        .body()
        .deserialize()
        .ok()?;
    let device = device.to_string();

    // "" is this user. no enrolled fingers is an error, not an empty list
    let fingers: Vec<String> = call_with(&device, DEVICE, "ListEnrolledFingers", "")
        .ok()?
        .body()
        .deserialize()
        .ok()?;

    if fingers.is_empty() {
        return None;
    }

    let owner: String = system()
        .ok()?
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "GetNameOwner",
            &(NAME,),
        )
        .ok()?
        .body()
        .deserialize()
        .ok()?;

    Some((owner, device))
}

// false with nothing to listen with, an error when the reader is worth another try
fn claim(round: u64) -> zbus::Result<bool> {
    let Some((owner, device)) = reader() else {
        return Ok(false);
    };

    call_with(&device, DEVICE, "Claim", "")?;

    {
        let mut claimed = lock(&CLAIMED);

        // the lock ended, or used up its tries, while this was on its way
        if !current(round) || spent() {
            drop(claimed);

            let _ = call(&device, DEVICE, "Release");

            return Ok(true);
        }

        *claimed = Some(Claim {
            round,
            owner,
            device: device.clone(),
        });
    }

    if let Err(error) = call_with(&device, DEVICE, "VerifyStart", "any") {
        if let Some(claim) = lock(&CLAIMED).take() {
            give_back(&claim);
        }

        return Err(error);
    }

    Ok(true)
}

// each watched again if its bus connection drops
fn watch() {
    WATCHING.call_once(|| {
        // made here, before the first verify starts, so its answer can't slip past
        let first = Bus::system().signals(DEVICE, "VerifyStatus");

        thread::spawn(move || {
            let mut signals = Some(first);

            loop {
                for signal in signals
                    .take()
                    .unwrap_or_else(|| Bus::system().signals(DEVICE, "VerifyStatus"))
                {
                    verified(&signal);
                }

                thread::sleep(RETRY_AFTER);
            }
        });

        thread::spawn(|| {
            loop {
                for signal in
                    Bus::system().signals("org.freedesktop.login1.Manager", "PrepareForSleep")
                {
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

    if signal.sender() != "org.freedesktop.DBus"
        || arguments.first().map_or("", crate::Value::text) != NAME
    {
        return;
    }

    let new_owner = arguments.get(2).map_or("", crate::Value::text);

    let ours = lock(&CLAIMED)
        .as_ref()
        .is_some_and(|claim| claim.owner != new_owner);

    if ours {
        lock(&CLAIMED).take();

        restart();
    }
}

// VerifyStatus(result, done)
fn verified(signal: &Signal) {
    let result = signal.arguments().first().map_or("", crate::Value::text);

    let (round, device, last) = {
        let mut claimed = lock(&CLAIMED);

        let Some(claim) = claimed.as_ref() else {
            return;
        };

        // anyone can send a signal with this name: only fprintd, about our reader, counts
        if signal.sender() != claim.owner || signal.path() != claim.device {
            return;
        }

        let (round, device) = (claim.round, claim.device.clone());

        // counted under CLAIMED, so no new claim slips in after the last try
        let last = result == "verify-no-match"
            && current(round)
            && MISSES.fetch_add(1, Ordering::SeqCst) + 1 >= MAX_MISSES;

        (round, device, if last { claimed.take() } else { None })
    };

    match result {
        "verify-match" => Lock::fingerprint_matched(round),

        // the last try: only the password unlocks this lock now
        "verify-no-match" if let Some(claim) = last => {
            Lock::fingerprint_changed(round, Fingerprint::TooManyTries);

            thread::spawn(move || {
                let _working = lock(&WORKING);

                give_back(&claim);
            });
        }

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
        "verify-retry-scan"
        | "verify-swipe-too-short"
        | "verify-finger-not-centered"
        | "verify-remove-and-retry" => {
            Lock::fingerprint_changed(round, Fingerprint::Retry);

            thread::spawn(move || settle(round, Fingerprint::Retry));
        }

        // the reader went away or broke, it's tried again
        _ => {
            Lock::fingerprint_changed(round, Fingerprint::Unavailable);

            restart();
        }
    }
}

// back to listening after a moment, unless something newer replaced the message
fn settle(round: u64, shown: Fingerprint) {
    thread::sleep(MESSAGE_FOR);

    Lock::fingerprint_settled(round, shown);
}
