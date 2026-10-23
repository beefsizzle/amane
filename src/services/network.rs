mod access_point;
mod link;
mod manager;
mod profile;
mod wifi;

use std::thread;
use std::time::{Duration, Instant};

use crate::services::worker;
use crate::{Service, Value};

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

/*
 * networkmanager looks for internet every five minutes, so a connection
 * that lost it for a moment (say networkmanager was restarted before dns
 * came back) would show none long after it returned. while it's missing,
 * networkmanager is asked to look again this often
 */
const RECHECK: Duration = Duration::from_secs(20);

/*
 * polled, because the access point's strength
 * changes all the time without a new connection
 */
impl Service for Network {
    fn new() -> Self {
        fetch()
    }

    /*
     * networkmanager is asked outside the lock, so a slow answer never
     * holds up a frame, and nothing is written when nothing changed, so
     * the windows aren't redrawn every second for no reason
     */
    fn listen() {
        let mut checked: Option<Instant> = None;

        loop {
            thread::sleep(Self::interval());

            let fresh = fetch();

            if fresh.connected() && !fresh.internet && checked.is_none_or(|at| at.elapsed() >= RECHECK) {
                checked = Some(Instant::now());

                worker::run(manager::check_connectivity);
            }

            if *Self::read() == fresh {
                continue;
            }

            *Self::write() = fresh;
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

    // connected and reaching the internet, as networkmanager's own check says
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

    let settings = profile::settings(ssid, password);

    manager::add_and_activate(settings, &device);
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
