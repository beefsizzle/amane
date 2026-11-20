use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::thread;
use std::time::Duration;

use inotify::{EventMask, Inotify, WatchDescriptor, WatchMask};

use super::walk;

// how long the folders must stay quiet before a rescan, a package install touches many files
const SETTLE: Duration = Duration::from_millis(500);

// a path that keeps pointing at itself is given up on, like the kernel does
const MAX_LINKS: usize = 40;

/*
 * one mask for every watch, since adding a watch on an inode that is
 * already watched replaces its mask rather than adding to it
 */
const MASK: WatchMask = WatchMask::CREATE
    .union(WatchMask::DELETE)
    .union(WatchMask::MOVED_FROM)
    .union(WatchMask::MOVED_TO)
    .union(WatchMask::CLOSE_WRITE)
    .union(WatchMask::DELETE_SELF)
    .union(WatchMask::MOVE_SELF);

// what a watched folder is watched for
#[derive(Default, Debug, PartialEq)]
struct Interest {
    // an applications folder or one inside it, where any desktop file counts
    apps: bool,

    // entries on the way to one, a folder that doesn't exist yet or a link that may be repointed
    names: HashSet<OsString>,
}

// the applications folders and the way to them, so the list is only scanned when one changes
pub struct Watch {
    inotify: Inotify,

    interests: HashMap<WatchDescriptor, Interest>,

    buffer: Vec<u8>,
}

impl Watch {
    pub fn new() -> Option<Self> {
        let inotify = Inotify::init().ok()?;

        Some(Self {
            inotify,
            interests: HashMap::new(),
            buffer: vec![0; 4096],
        })
    }

    /*
     * called before every scan, so a file written during the scan is never
     * missed, and a folder that appeared since the last one is watched too
     */
    pub fn follow(&mut self, applications: &[PathBuf]) {
        let mut watches = self.inotify.watches();

        let mut interests: HashMap<WatchDescriptor, Interest> = HashMap::new();

        for folder in applications {
            resolve(folder, &mut |folder, name| {
                let Ok(wd) = watches.add(folder, MASK) else {
                    return;
                };

                let interest = interests.entry(wd).or_default();

                match name {
                    Some(name) => {
                        interest.names.insert(name.to_owned());
                    }
                    None => interest.apps = true,
                }
            });
        }

        for (wd, _) in self.interests.drain() {
            if !interests.contains_key(&wd) {
                let _ = watches.remove(wd);
            }
        }

        self.interests = interests;
    }

    // blocks until a desktop file changed and the folders went quiet; false if inotify stopped working
    pub fn wait(&mut self) -> bool {
        loop {
            let Ok(events) = self.inotify.read_events_blocking(&mut self.buffer) else {
                return false;
            };

            if events
                .into_iter()
                .any(|event| counts(&self.interests, &event.wd, event.mask, event.name))
            {
                break;
            }
        }

        loop {
            thread::sleep(SETTLE);

            match self.drain() {
                Ok(true) => continue,
                Ok(false) => return true,
                Err(_) => return false,
            }
        }
    }

    // reads what came in while sleeping, true if any of it counts
    fn drain(&mut self) -> io::Result<bool> {
        let mut changed = false;

        loop {
            match self.inotify.read_events(&mut self.buffer) {
                Ok(events) => {
                    for event in events {
                        changed |= counts(&self.interests, &event.wd, event.mask, event.name);
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(changed),
                Err(error) => return Err(error),
            }
        }
    }
}

fn counts(
    interests: &HashMap<WatchDescriptor, Interest>,
    wd: &WatchDescriptor,
    mask: EventMask,
    name: Option<&OsStr>,
) -> bool {
    // the kernel dropped events, so anything may have changed
    if mask.contains(EventMask::Q_OVERFLOW) {
        return true;
    }

    let Some(interest) = interests.get(wd) else {
        return false;
    };

    // a watched folder went away, the way to it is watched again
    if mask.intersects(EventMask::DELETE_SELF | EventMask::MOVE_SELF) {
        return true;
    }

    let Some(name) = name else {
        return false;
    };

    if interest.names.contains(name) {
        return true;
    }

    /*
     * a folder holds desktop files too, but other files don't count, like
     * the mimeinfo.cache a package manager writes after every install
     */
    interest.apps
        && (mask.contains(EventMask::ISDIR)
            || Path::new(name).extension() == Some("desktop".as_ref()))
}

/*
 * hands add every folder to watch on the way to path: with the name it
 * waits for where a folder is missing or a link may be repointed, and
 * without one for the folder itself and every folder inside it. each watch
 * comes before the look, so an entry made in between still wakes it
 */
fn resolve(path: &Path, add: &mut impl FnMut(&Path, Option<&OsStr>)) {
    let mut left: Vec<OsString> = Vec::new();

    push_components(path, &mut left);

    let mut folder = if path.is_absolute() {
        PathBuf::from("/")
    } else {
        PathBuf::from(".")
    };

    let mut links = 0;

    while let Some(name) = left.pop() {
        if name == ".." {
            folder.pop();

            continue;
        }

        let next = folder.join(&name);

        let Ok(metadata) = fs::symlink_metadata(&next) else {
            add(&folder, Some(&name));

            // made since the first look, so the way goes on
            if fs::symlink_metadata(&next).is_err() {
                return;
            }

            left.push(name);

            continue;
        };

        if metadata.is_symlink() {
            links += 1;

            if links > MAX_LINKS {
                return;
            }

            add(&folder, Some(&name));

            let Ok(target) = fs::read_link(&next) else {
                return;
            };

            if target.is_absolute() {
                folder = PathBuf::from("/");
            }

            push_components(&target, &mut left);

            continue;
        }

        if !metadata.is_dir() {
            return;
        }

        folder = next;
    }

    for inside in walk::folders(&folder) {
        add(&inside, None);
    }
}

// the path's parts, last one first, so the next one to walk is popped off the end
fn push_components(path: &Path, left: &mut Vec<OsString>) {
    let parts: Vec<OsString> = path
        .components()
        .filter_map(|part| match part {
            Component::Normal(name) => Some(name.to_owned()),
            Component::ParentDir => Some(OsString::from("..")),
            _ => None,
        })
        .collect();

    left.extend(parts.into_iter().rev());
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    fn temp(name: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("amane-apps-{name}-{}", std::process::id()));

        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();

        fs::canonicalize(base).unwrap()
    }

    fn watched(path: &Path) -> Vec<(PathBuf, Option<OsString>)> {
        let mut found = Vec::new();

        resolve(path, &mut |folder, name| {
            found.push((folder.to_path_buf(), name.map(ToOwned::to_owned)));
        });

        found
    }

    #[test]
    fn a_missing_folder_is_waited_on_from_the_nearest_one_that_exists() {
        let base = temp("missing");

        let found = watched(&base.join("flatpak/exports/share/applications"));

        assert_eq!(found, vec![(base.clone(), Some(OsString::from("flatpak")))]);

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_link_is_watched_where_it_lives_and_where_it_points() {
        let base = temp("link");

        fs::create_dir_all(base.join("store/share/applications/kde")).unwrap();
        fs::create_dir_all(base.join("profile")).unwrap();
        symlink(base.join("store/share"), base.join("profile/share")).unwrap();

        let found = watched(&base.join("profile/share/applications"));

        let applications = base.join("store/share/applications");

        assert_eq!(
            found,
            vec![
                (base.join("profile"), Some(OsString::from("share"))),
                (applications.clone(), None),
                (applications.join("kde"), None),
            ]
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn only_desktop_files_and_folders_count() {
        let base = temp("counts");

        let applications = base.join("share/applications");

        let mut watch = Watch::new().unwrap();

        watch.follow(std::slice::from_ref(&applications));

        // making the missing folder counts, then it's watched itself
        fs::create_dir_all(&applications).unwrap();

        assert!(watch.wait());

        watch.follow(std::slice::from_ref(&applications));

        fs::write(applications.join("mimeinfo.cache"), b"").unwrap();

        assert!(!watch.drain().unwrap());

        fs::write(applications.join("editor.desktop"), b"").unwrap();

        assert!(watch.drain().unwrap());

        fs::remove_file(applications.join("editor.desktop")).unwrap();

        assert!(watch.drain().unwrap());

        let _ = fs::remove_dir_all(&base);
    }
}
