use crate::{Argument, Bus, Value};

pub const NAME: &str = "org.freedesktop.NetworkManager";

pub const PATH: &str = "/org/freedesktop/NetworkManager";

// networkmanager's state numbers: up on the local network only, and online with internet
const CONNECTED_LOCAL: f64 = 50.0;
const CONNECTED_GLOBAL: f64 = 70.0;

pub fn state() -> f64 {
    Bus::system().property(NAME, PATH, NAME, "State").number()
}

// joined to a network, whether or not it reaches the internet
pub fn connected(state: f64) -> bool {
    state >= CONNECTED_LOCAL
}

pub fn online(state: f64) -> bool {
    state == CONNECTED_GLOBAL
}

pub fn check_connectivity() {
    Bus::system().call(NAME, PATH, NAME, "CheckConnectivity", &[]);
}

// the connection that carries the default route, as a map of its properties
pub fn primary_connection() -> Value {
    let primary = Bus::system().property(NAME, PATH, NAME, "PrimaryConnection");

    properties(
        primary.text(),
        "org.freedesktop.NetworkManager.Connection.Active",
    )
}

// the wifi network a connection is joined to, as a map of its properties
pub fn access_point(path: &str) -> Value {
    properties(path, "org.freedesktop.NetworkManager.AccessPoint")
}

// the radio switch, which turns every wifi device off or on
pub fn wifi_enabled() -> bool {
    Bus::system()
        .property(NAME, PATH, NAME, "WirelessEnabled")
        .bool()
}

pub fn set_wifi(enabled: bool) {
    Bus::system().set_property(NAME, PATH, NAME, "WirelessEnabled", Argument::from(enabled));
}

// "/" lets networkmanager pick the access point itself
pub fn activate(profile: &str, device: &str) {
    let arguments = [
        Argument::Path(String::from(profile)),
        Argument::Path(String::from(device)),
        Argument::Path(String::from("/")),
    ];

    Bus::system().call(NAME, PATH, NAME, "ActivateConnection", &arguments);
}

pub fn properties(path: &str, interface: &str) -> Value {
    let arguments = [Argument::from(interface)];

    Bus::system().call(
        NAME,
        path,
        "org.freedesktop.DBus.Properties",
        "GetAll",
        &arguments,
    )
}

#[cfg(test)]
mod tests {
    use super::{connected, online};

    #[test]
    fn a_connection_without_internet_is_still_connected() {
        // connected (local only), connected (site only), connected
        for state in [50.0, 60.0, 70.0] {
            assert!(connected(state));
        }

        assert!(!online(60.0));
        assert!(online(70.0));
    }

    #[test]
    fn joining_and_leaving_are_not_connected() {
        // asleep, disconnected, disconnecting, connecting
        for state in [10.0, 20.0, 30.0, 40.0] {
            assert!(!connected(state));
        }
    }
}
