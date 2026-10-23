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
 * the wifi device to show and join with. most machines have one, but a
 * laptop with a usb adapter next to its own card has two, and the first
 * one listed can be the idle one. so the device that is joined (or
 * joining) wins, then the one that sees the most networks
 */
pub fn device() -> Option<String> {
    let devices = Bus::system().call(NAME, PATH, NAME, "GetDevices", &[]);

    let mut best: Option<(String, Rank)> = None;

    for device in devices.list() {
        let path = device.text();

        let kind = Bus::system().property(NAME, path, DEVICE, "DeviceType");

        if kind.number() != WIFI_DEVICE {
            continue;
        }

        let state = Bus::system().property(NAME, path, DEVICE, "State");

        let seen = Bus::system().call(NAME, path, WIRELESS, "GetAllAccessPoints", &[]);

        let rank = rank(state.number(), seen.list().len());

        // the first listed stays on a tie
        if best.as_ref().is_none_or(|(_, top)| rank > *top) {
            best = Some((String::from(path), rank));
        }
    }

    best.map(|(path, _)| path)
}

// in use first, then how many networks it sees
type Rank = (bool, usize);

fn rank(state: f64, seen: usize) -> Rank {
    ((PREPARING..=ACTIVATED).contains(&state), seen)
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
    use super::rank;

    #[test]
    fn the_device_in_use_wins_over_one_that_sees_more() {
        // joined to a network, and an idle card that sees more
        assert!(rank(100.0, 3) > rank(30.0, 12));

        // still joining counts as in use
        assert!(rank(50.0, 0) > rank(30.0, 12));
    }

    #[test]
    fn otherwise_the_device_that_sees_more_networks_wins() {
        assert!(rank(30.0, 8) > rank(30.0, 0));

        // a device that is off or unavailable is no better than an idle one
        assert!(rank(20.0, 8) > rank(30.0, 0));
    }
}
