use std::path::{Path, PathBuf};
use std::process::Command;

use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_feedback_v1::{
    self, ZwpLinuxDmabufFeedbackV1,
};
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1;

/*
 * the Vulkan loader starts every installed driver, even for a gpu the shell never draws on. the filter has to be in
 * the shell's environment before it starts, so `amane run` and `amane dev` put it there, and the shell keeps it from
 * the programs it starts, which may want another gpu
 */

// tells the shell the filter came from amane, not the user
const MARKER: &str = "AMANE_VULKAN_FILTER";

// starts the shell with only the Vulkan driver of the compositor's gpu, unless a driver choice is already set
pub fn limit_vulkan_drivers(shell: &mut Command) {
    if let Some(filter) = driver_filter() {
        shell
            .env("VK_LOADER_DRIVERS_SELECT", filter)
            .env(MARKER, "1");
    }
}

// takes amane's filter back off a program the shell starts; one the user set stays
pub(crate) fn leave_out_filter(command: &mut Command) {
    leave_out(command, std::env::var_os(MARKER).is_some());
}

fn leave_out(command: &mut Command, ours: bool) {
    if ours {
        command
            .env_remove("VK_LOADER_DRIVERS_SELECT")
            .env_remove(MARKER);
    }
}

// the VK_LOADER_DRIVERS_SELECT value for the compositor's gpu, like `*intel*`
fn driver_filter() -> Option<String> {
    let chosen = [
        "VK_LOADER_DRIVERS_SELECT",
        "VK_LOADER_DRIVERS_DISABLE",
        "VK_DRIVER_FILES",
        "VK_ICD_FILENAMES",
        "VK_ADD_DRIVER_FILES",
    ];

    if chosen.iter().any(|name| std::env::var_os(name).is_some()) {
        return None;
    }

    let kernel = kernel_driver(compositor_device()?)?;

    filter(&kernel, &installed_manifests())
}

// none when no manifest matches, or when every one does
fn filter(kernel: &str, manifests: &[String]) -> Option<String> {
    let names = vulkan_names(kernel)?;

    let matches = |manifest: &String| names.iter().any(|name| manifest.contains(name));

    let matching = manifests
        .iter()
        .filter(|manifest| matches(manifest))
        .count();

    if matching == 0 || matching == manifests.len() {
        return None;
    }

    Some(
        names
            .iter()
            .map(|name| format!("*{name}*"))
            .collect::<Vec<_>>()
            .join(","),
    )
}

// what a kernel driver's Vulkan drivers are called in their manifest file names
fn vulkan_names(kernel: &str) -> Option<&'static [&'static str]> {
    match kernel {
        // intel_icd, and intel_hasvk_icd for gen 7 and 8
        "i915" | "xe" => Some(&["intel"]),
        // radeon_icd (radv), and amd_icd64 for AMDVLK
        "amdgpu" | "radeon" => Some(&["radeon", "amd_icd"]),
        "nvidia" => Some(&["nvidia"]),
        "nouveau" => Some(&["nouveau"]),
        "msm" => Some(&["freedreno"]),
        "panfrost" | "panthor" => Some(&["panfrost"]),
        "v3d" | "vc4" => Some(&["broadcom"]),
        "asahi" => Some(&["asahi"]),
        _ => None,
    }
}

// the kernel driver of a device number, packed as glibc's makedev does
fn kernel_driver(device: u64) -> Option<String> {
    let major = ((device >> 8) & 0xfff) | ((device >> 32) & !0xfff);
    let minor = (device & 0xff) | ((device >> 12) & !0xff);

    let driver = std::fs::read_link(format!("/sys/dev/char/{major}:{minor}/device/driver")).ok()?;

    Some(driver.file_name()?.to_string_lossy().into_owned())
}

// the Vulkan driver manifests' file names, from the folders the loader reads
fn installed_manifests() -> Vec<String> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();

    let list = |name: &str, default: &str| {
        std::env::var(name)
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| default.to_string())
    };

    let mut folders: Vec<PathBuf> = Vec::new();

    for base in list("XDG_CONFIG_DIRS", "/etc/xdg").split(':') {
        folders.push(Path::new(base).join("vulkan/icd.d"));
    }

    folders.push(PathBuf::from("/etc/vulkan/icd.d"));

    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/share"));

    folders.push(data_home.join("vulkan/icd.d"));

    for base in list("XDG_DATA_DIRS", "/usr/local/share:/usr/share").split(':') {
        folders.push(Path::new(base).join("vulkan/icd.d"));
    }

    // nixos keeps its drivers here, and its loader is patched to read it
    folders.push(PathBuf::from("/run/opengl-driver/share/vulkan/icd.d"));

    let mut manifests: Vec<String> = folders
        .iter()
        .filter_map(|folder| std::fs::read_dir(folder).ok())
        .flatten()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".json"))
        .collect();

    manifests.sort();
    manifests.dedup();

    manifests
}

#[derive(Default)]
struct Feedback {
    device: Option<u64>,
    done: bool,
}

// the gpu linux-dmabuf's default feedback names as the compositor's main device
fn compositor_device() -> Option<u64> {
    let connection = Connection::connect_to_env().ok()?;

    let (globals, mut queue) = registry_queue_init::<Feedback>(&connection).ok()?;

    // the feedback came with version 4
    let dmabuf: ZwpLinuxDmabufV1 = globals.bind(&queue.handle(), 4..=4, ()).ok()?;

    let feedback = dmabuf.get_default_feedback(&queue.handle(), ());

    let mut state = Feedback::default();

    while !state.done {
        queue.blocking_dispatch(&mut state).ok()?;
    }

    feedback.destroy();
    dmabuf.destroy();

    state.device
}

impl Dispatch<WlRegistry, GlobalListContents> for Feedback {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as wayland_client::Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpLinuxDmabufV1, ()> for Feedback {
    fn event(
        _: &mut Self,
        _: &ZwpLinuxDmabufV1,
        _: <ZwpLinuxDmabufV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpLinuxDmabufFeedbackV1, ()> for Feedback {
    fn event(
        state: &mut Self,
        _: &ZwpLinuxDmabufFeedbackV1,
        event: zwp_linux_dmabuf_feedback_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            // a dev_t in the machine's own byte order
            zwp_linux_dmabuf_feedback_v1::Event::MainDevice { device } => {
                state.device = device.as_slice().try_into().ok().map(u64::from_ne_bytes);
            }
            zwp_linux_dmabuf_feedback_v1::Event::Done => state.done = true,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::process::Command;

    use super::{MARKER, filter, leave_out};

    fn manifests(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_string()).collect()
    }

    // what a command will take away from its environment
    fn removed(command: &Command) -> Vec<&OsStr> {
        command
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(key, _)| key)
            .collect()
    }

    #[test]
    fn programs_the_shell_starts_lose_only_amanes_own_filter() {
        let mut started = Command::new("true");

        leave_out(&mut started, true);

        assert_eq!(
            removed(&started),
            [OsStr::new(MARKER), OsStr::new("VK_LOADER_DRIVERS_SELECT")]
        );

        // the user's own filter, without the marker
        let mut started = Command::new("true");

        leave_out(&mut started, false);

        assert!(removed(&started).is_empty());
    }

    #[test]
    fn a_laptop_with_intel_and_nvidia_keeps_only_intel() {
        let installed = manifests(&[
            "intel_hasvk_icd.x86_64.json",
            "intel_icd.x86_64.json",
            "nvidia_icd.json",
        ]);

        assert_eq!(filter("i915", &installed).as_deref(), Some("*intel*"));
    }

    #[test]
    fn a_compositor_on_nvidia_keeps_only_nvidia() {
        let installed = manifests(&["intel_icd.x86_64.json", "nvidia_icd.json"]);

        assert_eq!(filter("nvidia", &installed).as_deref(), Some("*nvidia*"));
    }

    #[test]
    fn amd_keeps_radv_and_amdvlk() {
        let installed = manifests(&[
            "amd_icd64.json",
            "intel_icd.x86_64.json",
            "lvp_icd.x86_64.json",
            "radeon_icd.x86_64.json",
        ]);

        assert_eq!(
            filter("amdgpu", &installed).as_deref(),
            Some("*radeon*,*amd_icd*")
        );
    }

    #[test]
    fn nothing_is_filtered_when_it_would_leave_no_driver_or_save_nothing() {
        // no Vulkan driver for this gpu installed: the shell keeps whatever there is
        assert_eq!(
            filter(
                "i915",
                &manifests(&["nvidia_icd.json", "lvp_icd.x86_64.json"])
            ),
            None
        );

        // only this gpu's driver installed
        assert_eq!(filter("i915", &manifests(&["intel_icd.x86_64.json"])), None);

        // a gpu that isn't known
        assert_eq!(
            filter(
                "virtio_gpu",
                &manifests(&["intel_icd.x86_64.json", "virtio_icd.x86_64.json"])
            ),
            None
        );
    }
}
