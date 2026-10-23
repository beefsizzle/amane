use std::collections::BTreeMap;

use crate::{Argument, Bus};

use super::AccessPoint;
use super::manager::{self, NAME, PATH};
use super::profile;

const DEVICE: &str = "org.freedesktop.NetworkManager.Device";

const WIRELESS: &str = "org.freedesktop.NetworkManager.Device.Wireless";

// networkmanager's own number for a wifi device
const WIFI_DEVICE: f64 = 2.0;

// the device states from picking a network up to being joined to it
const PREPARING: f64 = 40.0;
const ACTIVATED: f64 = 100.0;

/*
 * the wifi device to show and join with: the one joined or joining, else
 * the one that sees the most networks. with one device nothing is asked
 */
pub fn device() -> Option<String> {
    let devices = Bus::system().call(NAME, PATH, NAME, "GetDevices", &[]);

    let mut wifi = Vec::new();

    for device in devices.list() {
        let path = device.text();

        let kind = Bus::system().property(NAME, path, DEVICE, "DeviceType");

        if kind.number() == WIFI_DEVICE {
            wifi.push(String::from(path));
        }
    }

    if wifi.len() < 2 {
        return wifi.pop();
    }

    let states: Vec<f64> = wifi
        .iter()
        .map(|path| Bus::system().property(NAME, path, DEVICE, "State").number())
        .collect();

    let seen = |index: usize| {
        Bus::system()
            .call(NAME, &wifi[index], WIRELESS, "GetAllAccessPoints", &[])
            .list()
            .len()
    };

    let chosen = pick(&states, seen);

    Some(wifi.swap_remove(chosen))
}

fn pick(states: &[f64], seen: impl Fn(usize) -> usize) -> usize {
    if let Some(in_use) = states
        .iter()
        .position(|state| (PREPARING..=ACTIVATED).contains(state))
    {
        return in_use;
    }

    let mut best = (0, 0);

    for index in 0..states.len() {
        let count = seen(index);

        if index == 0 || count > best.1 {
            best = (index, count);
        }
    }

    best.0
}

// the networks the device saw in its last scan, strongest first
pub fn access_points(device: &str) -> Vec<AccessPoint> {
    let active = Bus::system().property(NAME, device, WIRELESS, "ActiveAccessPoint");

    let paths = Bus::system().call(NAME, device, WIRELESS, "GetAllAccessPoints", &[]);

    let mut access_points = Vec::new();

    for path in paths.list() {
        let properties = manager::access_point(path.text());

        let is_active = path.text() == active.text();

        let access_point = AccessPoint::from_properties(&properties, is_active);

        // hidden networks have no name to show or join by
        if access_point.ssid.is_empty() {
            continue;
        }

        add(&mut access_points, access_point);
    }

    let saved = profile::saved_names();

    for access_point in &mut access_points {
        access_point.saved = saved.contains(&access_point.ssid);
    }

    access_points.sort_by_key(|a| std::cmp::Reverse(a.strength));

    access_points
}

// still joining a network, not yet up
pub fn connecting(device: &str) -> bool {
    let state = Bus::system()
        .property(NAME, device, DEVICE, "State")
        .number();

    (PREPARING..ACTIVATED).contains(&state)
}

/*
 * one network often has several access points,
 * so they are merged into one entry per name
 */
fn add(access_points: &mut Vec<AccessPoint>, access_point: AccessPoint) {
    let Some(same) = access_points
        .iter_mut()
        .find(|existing| existing.ssid == access_point.ssid)
    else {
        access_points.push(access_point);

        return;
    };

    same.strength = u8::max(same.strength, access_point.strength);
    same.secured |= access_point.secured;
    same.active |= access_point.active;
    same.enterprise |= access_point.enterprise;

    // one access point that still takes wpa2 is enough to join with it
    same.wpa3_only &= access_point.wpa3_only;
}

// the results show up in access_points a few seconds later
pub fn scan(device: &str) {
    let options = Argument::Map(BTreeMap::new());

    Bus::system().call(NAME, device, WIRELESS, "RequestScan", &[options]);
}

pub fn disconnect(device: &str) {
    Bus::system().call(NAME, device, DEVICE, "Disconnect", &[]);
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::pick;

    #[test]
    fn the_device_in_use_wins_without_counting_networks() {
        let counted = Cell::new(0);

        let seen = |_| {
            counted.set(counted.get() + 1);
            12
        };

        // an idle card, then one joined to a network
        assert_eq!(pick(&[30.0, 100.0], seen), 1);

        // still joining counts as in use
        assert_eq!(pick(&[30.0, 50.0], seen), 1);

        assert_eq!(counted.get(), 0);
    }

    #[test]
    fn otherwise_the_device_that_sees_more_networks_wins() {
        let seen = |index| [0, 8][index];

        assert_eq!(pick(&[30.0, 30.0], seen), 1);

        // an unavailable device is no better than an idle one, and a tie goes to the first
        assert_eq!(pick(&[20.0, 30.0], |index| [8, 0][index]), 0);
        assert_eq!(pick(&[30.0, 30.0], |_| 5), 0);
    }
}
