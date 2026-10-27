mod access_point;
mod link;
mod manager;
mod profile;
mod wifi;

use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use crate::dbus::Signal;
use crate::services::worker;
use crate::{Bus, Service, Value};

pub use access_point::AccessPoint;
pub use link::Link;

#[derive(Default, PartialEq)]
pub struct Network {
    link: Link,

    // empty on a wired link
    ssid: String,

    // 0 to 100, and 0 on a wired link
    strength: u8,

    wifi_enabled: bool,

    // false while connected to a network that doesn't reach the internet
    internet: bool,

    // joining a wifi network that isn't up yet
    connecting: bool,

    // empty without a wifi device, or with the radio off
    access_points: Vec<AccessPoint>,
}

// networkmanager's own check only runs every five minutes
const RECHECK: Duration = Duration::from_secs(20);

// networkmanager's objects all live under this path
const PATH: &str = "/org/freedesktop/NetworkManager";

// changes nothing here shows, which some cards send every few seconds
const UNSHOWN: [&str; 2] = ["Bitrate", "LastSeen"];

// a burst of changes, like joining a network, is fetched once it has been quiet this long, or after a second at most
const SETTLE: Duration = Duration::from_millis(200);
const SETTLE_AT_MOST: Duration = Duration::from_secs(1);

// how long a lost bus is left before the changes are watched again
const REWATCH: Duration = Duration::from_secs(5);

/*
 * fetched again when networkmanager announces a change, the access
 * point's strength included, so an idle connection costs nothing
 */
impl Service for Network {
    fn new() -> Self {
        fetch()
    }

    /*
     * networkmanager is asked outside the lock, so a slow answer never
     * holds up a frame, and nothing is written when nothing changed
     */
    fn listen() {
        let mut changes = watch();
        let mut checked: Option<Instant> = None;

        loop {
            let fresh = fetch();

            let without_internet = fresh.connected() && !fresh.internet;

            if without_internet && checked.is_none_or(|at| at.elapsed() >= RECHECK) {
                checked = Some(Instant::now());

                worker::run(manager::check_connectivity);
            }

            if *Self::read() != fresh {
                *Self::write() = fresh;
            }

            // without internet the check above comes round again, even when nothing changes
            let next = if without_internet {
                changes.recv_timeout(RECHECK)
            } else {
                changes.recv().map_err(|_| RecvTimeoutError::Disconnected)
            };

            if next == Err(RecvTimeoutError::Disconnected) {
                thread::sleep(REWATCH);

                changes = watch();

                continue;
            }

            settle(&changes);
        }
    }
}

impl Network {
    pub fn connected(&self) -> bool {
        self.link != Link::Offline
    }

    pub fn link(&self) -> Link {
        self.link
    }

    pub fn ssid(&self) -> &str {
        &self.ssid
    }

    pub fn strength(&self) -> u8 {
        self.strength
    }

    pub fn wifi_enabled(&self) -> bool {
        self.wifi_enabled
    }

    // reaching the internet, by networkmanager's own check
    pub fn internet(&self) -> bool {
        self.internet
    }

    pub fn connecting(&self) -> bool {
        self.connecting
    }

    pub fn access_points(&self) -> &[AccessPoint] {
        &self.access_points
    }
}

/*
 * these talk to networkmanager right away, and the
 * next update shows what changed
 */
impl Network {
    // new networks show up in access_points a few seconds later
    pub fn scan() {
        worker::run(|| {
            let Some(device) = wifi::device() else {
                return;
            };

            wifi::scan(&device);
        });
    }

    pub fn connect(ssid: &str, password: Option<&str>) {
        let ssid = String::from(ssid);
        let password = password.map(String::from);

        worker::run(move || join(&ssid, password.as_deref()));
    }

    pub fn disconnect() {
        worker::run(|| {
            let Some(device) = wifi::device() else {
                return;
            };

            wifi::disconnect(&device);
        });
    }

    pub fn set_wifi(enabled: bool) {
        worker::run(move || manager::set_wifi(enabled));
    }

    // drops the saved profile and its password, so joining asks again
    pub fn forget(ssid: &str) {
        let ssid = String::from(ssid);

        worker::run(move || profile::forget(&ssid));
    }
}

/*
 * a saved profile already holds its password, so it is joined as it is;
 * otherwise networkmanager saves a new one, password included
 */
fn join(ssid: &str, password: Option<&str>) {
    let Some(device) = wifi::device() else {
        return;
    };

    if let Some(profile) = profile::find(ssid) {
        manager::activate(&profile, &device);

        return;
    }

    let wpa3 = wifi::access_points(&device)
        .iter()
        .any(|access_point| access_point.ssid == ssid && access_point.wpa3_only);

    let Some(profile) = profile::add(profile::settings(ssid, password, wpa3)) else {
        return;
    };

    manager::activate(&profile, &device);
}

// networkmanager's changes, watched before the first fetch so none slips past; ends with the bus
fn watch() -> Receiver<()> {
    let signals = Bus::system().signals("org.freedesktop.DBus.Properties", "PropertiesChanged");

    let (sender, changes) = mpsc::channel();

    thread::spawn(move || {
        for signal in signals {
            if signal.path().starts_with(PATH) && shown(&signal) && sender.send(()).is_err() {
                return;
            }
        }
    });

    changes
}

// the arguments are the interface, the changed properties and the invalidated ones
fn shown(signal: &Signal) -> bool {
    match signal.arguments().get(1) {
        Some(Value::Map(changed)) if !changed.is_empty() => {
            changed.keys().any(|name| !UNSHOWN.contains(&name.as_str()))
        }
        _ => true,
    }
}

fn settle(changes: &Receiver<()>) {
    let started = Instant::now();

    while started.elapsed() < SETTLE_AT_MOST && changes.recv_timeout(SETTLE).is_ok() {}
}

// the ssid comes as raw bytes, and is almost always utf-8
fn ssid(bytes: &Value) -> String {
    let mut raw = Vec::new();

    for byte in bytes.list() {
        raw.push(byte.number() as u8);
    }

    String::from_utf8_lossy(&raw).into_owned()
}

// asks networkmanager for everything shown, from scratch
fn fetch() -> Network {
    // networks to join are listed even while offline
    let mut network = Network {
        wifi_enabled: manager::wifi_enabled(),
        ..Network::default()
    };

    if let Some(device) = wifi::device() {
        network.access_points = wifi::access_points(&device);
        network.connecting = wifi::connecting(&device);
    }

    let state = manager::state();

    if !manager::connected(state) {
        return network;
    }

    network.internet = manager::online(state);

    let connection = manager::primary_connection();

    network.link = Link::from_type(connection.get("Type").text());

    if network.link != Link::Wifi {
        return network;
    }

    let access_point = manager::access_point(connection.get("SpecificObject").text());

    network.ssid = ssid(access_point.get("Ssid"));
    network.strength = access_point.get("Strength").number() as u8;

    network
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::shown;
    use crate::Value;
    use crate::dbus::Signal;

    fn changed(names: &[&str]) -> Signal {
        let properties = names
            .iter()
            .map(|name| (String::from(*name), Value::Nothing))
            .collect::<BTreeMap<_, _>>();

        Signal {
            sender: String::from(":1.4"),
            path: String::from("/org/freedesktop/NetworkManager/Devices/3"),
            arguments: vec![
                Value::Text(String::from(
                    "org.freedesktop.NetworkManager.Device.Wireless",
                )),
                Value::Map(properties),
                Value::List(Vec::new()),
            ],
        }
    }

    #[test]
    fn a_change_nothing_shows_is_left_alone() {
        assert!(!shown(&changed(&["Bitrate"])));
        assert!(!shown(&changed(&["Bitrate", "LastSeen"])));
    }

    #[test]
    fn a_change_to_anything_shown_fetches_again() {
        assert!(shown(&changed(&["Strength"])));
        assert!(shown(&changed(&["Bitrate", "State"])));

        // only invalidated properties, which say nothing about what changed
        assert!(shown(&changed(&[])));
    }
}
