use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, Once, PoisonError};
use std::thread;

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

// the reader this lock holds, and who may speak for it
struct Claim {
    round: u64,
    owner: String, // fprintd's unique name, like :1.83
    device: String,
}

static CLAIMED: Mutex<Option<Claim>> = Mutex::new(None);

static LISTENING: Once = Once::new();

pub(super) fn enable() {
    ENABLED.store(true, Ordering::Relaxed);
}

/// the round of the lock that's on now, for checking an answer still belongs to it
pub(super) fn current(round: u64) -> bool {
    ROUND.load(Ordering::SeqCst) == round
}

// a lock started: claim the reader and wait for a finger, unless fingerprints are off
pub(super) fn begin() {
    let round = ROUND.fetch_add(1, Ordering::SeqCst) + 1;

    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }

    // fprintd may have to start first, and claiming the reader is a few calls; none of it may hold up the lock
    thread::spawn(move || {
        let state = match claim(round) {
            Ok(true) => Fingerprint::Listening,
            Ok(false) | Err(_) => Fingerprint::Unavailable,
        };

        Lock::fingerprint_changed(round, state);
    });
}

// the lock ended: give the reader back
pub(super) fn end() {
    ROUND.fetch_add(1, Ordering::SeqCst);

    release();
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

// false when there's nothing to listen with: no reader, or no finger enrolled for this user
fn claim(round: u64) -> zbus::Result<bool> {
    // asking for the reader also starts fprintd, which runs on demand
    let device: OwnedObjectPath = call(MANAGER_PATH, MANAGER, "GetDefaultDevice")?.body().deserialize()?;
    let device = device.to_string();

    // "" is the user this program runs as
    let fingers: Vec<String> = call_with(&device, DEVICE, "ListEnrolledFingers", "")?.body().deserialize()?;

    if fingers.is_empty() {
        return Ok(false);
    }

    let owner: String = system()?
        .call_method(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", Some("org.freedesktop.DBus"), "GetNameOwner", &(NAME,))?
        .body()
        .deserialize()?;

    listen();

    // the claim belongs to this connection, and ends with it
    call_with(&device, DEVICE, "Claim", "")?;

    {
        let mut claimed = CLAIMED.lock().unwrap_or_else(PoisonError::into_inner);

        // the lock ended while this was on its way
        if !current(round) {
            drop(claimed);

            let _ = call(&device, DEVICE, "Release");

            return Ok(true);
        }

        *claimed = Some(Claim { round, owner, device: device.clone() });
    }

    call_with(&device, DEVICE, "VerifyStart", "any")?;

    Ok(true)
}

// one thread for the program's whole life, made the first time a reader is claimed
fn listen() {
    LISTENING.call_once(|| {
        // watching before the first verify starts, so its answer can't slip past
        let signals = Bus::system().signals(DEVICE, "VerifyStatus");

        thread::spawn(move || {
            for signal in signals {
                verified(&signal);
            }
        });
    });
}

// VerifyStatus(result, done)
fn verified(signal: &Signal) {
    let (round, device) = {
        let claimed = CLAIMED.lock().unwrap_or_else(PoisonError::into_inner);

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
            });
        }

        // a poor scan, fprintd keeps listening
        "verify-retry-scan" | "verify-swipe-too-short" | "verify-finger-not-centered" | "verify-remove-and-retry" => {
            Lock::fingerprint_changed(round, Fingerprint::Retry);
        }

        // the reader went away or broke, the password still works
        _ => {
            Lock::fingerprint_changed(round, Fingerprint::Unavailable);

            release();
        }
    }
}

// stops listening and gives the reader back, if one is held
fn release() {
    let Some(claim) = CLAIMED.lock().unwrap_or_else(PoisonError::into_inner).take() else { return };

    thread::spawn(move || {
        let _ = call(&claim.device, DEVICE, "VerifyStop");
        let _ = call(&claim.device, DEVICE, "Release");
    });
}
