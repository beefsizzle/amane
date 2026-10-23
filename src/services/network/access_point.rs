use crate::Value;

use super::ssid;

// networkmanager's flag for a network that asks for a password
const PRIVACY: u32 = 1;

// networkmanager's key management flags, in WpaFlags and RsnFlags: a password (wpa2), a username and password
// (enterprise, 802.1x), and wpa3's password handshake
const KEY_MGMT_PSK: u32 = 0x100;
const KEY_MGMT_802_1X: u32 = 0x200;
const KEY_MGMT_SAE: u32 = 0x400;

// a wifi network the device can see
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessPoint {
    pub(crate) ssid: String,

    // 0 to 100
    pub(crate) strength: u8,

    pub(crate) secured: bool,

    // the network the device is joined to right now
    pub(crate) active: bool,

    // networkmanager has a profile for it, password included
    pub(crate) saved: bool,

    // logs in with a username as well as a password (802.1x), which connect() can't do
    pub(crate) enterprise: bool,

    // takes a password through wpa3 only, which a wpa2 profile can't join
    pub(crate) wpa3_only: bool,
}

impl AccessPoint {
    pub(crate) fn from_properties(properties: &Value, active: bool) -> Self {
        let flags = properties.get("Flags").number() as u32;
        let wpa = properties.get("WpaFlags").number() as u32;
        let rsn = properties.get("RsnFlags").number() as u32;

        let (secured, enterprise, wpa3_only) = security(flags, wpa, rsn);

        Self {
            ssid: ssid(properties.get("Ssid")),
            strength: properties.get("Strength").number() as u8,
            secured,
            active,
            saved: false,
            enterprise,
            wpa3_only,
        }
    }

    pub fn ssid(&self) -> &str {
        &self.ssid
    }

    pub fn strength(&self) -> u8 {
        self.strength
    }

    pub fn secured(&self) -> bool {
        self.secured
    }

    pub fn active(&self) -> bool {
        self.active
    }

    pub fn saved(&self) -> bool {
        self.saved
    }

    pub fn enterprise(&self) -> bool {
        self.enterprise
    }
}

// whether it asks for a password, for a username too, and for wpa3 only. the privacy flag alone means old wep, wpa
// and rsn cover everything newer
fn security(flags: u32, wpa: u32, rsn: u32) -> (bool, bool, bool) {
    let keys = wpa | rsn;

    let secured = flags & PRIVACY != 0 || wpa != 0 || rsn != 0;

    (secured, keys & KEY_MGMT_802_1X != 0, keys & KEY_MGMT_SAE != 0 && keys & KEY_MGMT_PSK == 0)
}

#[cfg(test)]
mod tests {
    use super::security;

    #[test]
    fn security_kinds_come_from_the_key_flags() {
        // open, wep, wpa2, wpa2 and wpa3 side by side, wpa3 only, enterprise
        assert_eq!(security(0, 0, 0), (false, false, false));
        assert_eq!(security(1, 0, 0), (true, false, false));
        assert_eq!(security(1, 0, 0x188), (true, false, false));
        assert_eq!(security(1, 0, 0x588), (true, false, false));
        assert_eq!(security(1, 0, 0x488), (true, false, true));
        assert_eq!(security(1, 0, 0x288), (true, true, false));
    }
}
