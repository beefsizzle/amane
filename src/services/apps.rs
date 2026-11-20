mod data_dirs;
mod desktop_app;
mod exec;
pub(super) mod icons;
mod parse;
mod walk;
mod watch;

use std::collections::HashSet;
use std::thread;
use std::time::Duration;

use crate::Service;

use watch::Watch;

pub use desktop_app::DesktopApp;

// the programs a launcher can show, read from the .desktop files in the xdg data dirs
#[derive(Default)]
pub struct Apps {
    list: Vec<DesktopApp>,
}

// rescanned when a desktop file changes, so programs installed while the shell runs show up
impl Service for Apps {
    // empty at first: the scan walks every icon theme, which takes seconds
    fn new() -> Self {
        Self::default()
    }

    // only without inotify, which otherwise wakes the scan itself
    fn interval() -> Duration {
        Duration::from_secs(30)
    }

    fn listen() {
        let mut watch = Watch::new();

        loop {
            let folders: Vec<_> = data_dirs::find()
                .into_iter()
                .map(|dir| dir.join("applications"))
                .collect();

            if let Some(watch) = &mut watch {
                watch.follow(&folders);
            }

            rescan();

            let woken = watch.as_mut().is_some_and(Watch::wait);

            // inotify couldn't be set up or stopped working, so the list is polled
            if !woken {
                watch = None;

                thread::sleep(Self::interval());
            }
        }
    }
}

// scanned outside the lock, so a view reading the list never waits on the disk
fn rescan() {
    let list = scan();

    let mut apps = Apps::write();

    if apps.list == list {
        apps.quiet();
    } else {
        apps.list = list;
    }
}

impl Apps {
    // sorted by name, without the ones marked hidden or not to be shown
    pub fn list(&self) -> &[DesktopApp] {
        &self.list
    }
}

fn scan() -> Vec<DesktopApp> {
    let icon_folders = icons::read_icon_folders();

    // a file in an earlier dir overrides one with the same id in a later dir
    let mut seen = HashSet::new();

    let mut apps = Vec::new();

    for dir in data_dirs::find() {
        let applications = dir.join("applications");

        for path in walk::files(&applications) {
            let Some(id) = parse::id(&applications, &path) else {
                continue;
            };

            if !seen.insert(id) {
                continue;
            }

            let Some(app) = parse::read(&path, &icon_folders) else {
                continue;
            };

            apps.push(app);
        }
    }

    apps.sort_by_key(|app| app.name().to_lowercase());

    apps
}
