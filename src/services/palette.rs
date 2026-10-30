mod median_cut;
mod pick;
mod sample;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, SystemTime};

use inotify::{Inotify, WatchMask, Watches};

use crate::graphics::image;
use crate::{Color, Service};

// the colors used until an image is opened
const FALLBACK_BACKGROUND: Color = Color::rgb(0x11, 0x11, 0x1b);
const FALLBACK_ACCENT: Color = Color::rgb(0x89, 0xb4, 0xfa);

// colors taken from an image, usually the wallpaper, most common first
#[derive(Default)]
pub struct Palette {
    path: Option<PathBuf>,

    count: usize,

    // when the file was last read, so a rewrite of the same path is noticed
    modified: Option<SystemTime>,

    colors: Vec<Color>,
}

// lets open() watch the folder of a new image, while listen() waits for changes
static WATCHES: OnceLock<Watches> = OnceLock::new();

// a new file at the same path is picked up, and a new path through open()
impl Service for Palette {
    fn new() -> Self {
        Self::default()
    }

    // only without inotify, which is otherwise woken by the file itself
    fn interval() -> Duration {
        Duration::from_millis(500)
    }

    /*
     * the folder is watched rather than the file, since a file that is
     * replaced by a rename takes a watch on itself away with it
     */
    fn listen() {
        let Ok(mut inotify) = Inotify::init() else {
            return poll();
        };

        let _ = WATCHES.set(inotify.watches());

        // opened before the watches existed
        if let Some(path) = Self::read().path.clone() {
            watch_folder(&path);
        }

        let mut buffer = [0; 4096];

        loop {
            let Ok(events) = inotify.read_events_blocking(&mut buffer) else {
                return poll();
            };

            let path = Self::read().path.clone();

            let names: Vec<_> = path
                .as_deref()
                .map(files)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|file| file.file_name().map(ToOwned::to_owned))
                .collect();

            if events.into_iter().any(|event| {
                event
                    .name
                    .is_some_and(|name| names.iter().any(|known| known == name))
            }) {
                reread();
            }
        }
    }
}

fn poll() {
    loop {
        thread::sleep(Palette::interval());

        reread();
    }
}

// the image's folder, and for a link the folder of the file it points to
fn watch_folder(path: &Path) {
    let Some(watches) = WATCHES.get() else {
        return;
    };

    for file in files(path) {
        let folder = match file.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
        };

        // a written file is closed, a replaced one is moved into place, a link is made again
        let _ = watches.clone().add(
            folder,
            WatchMask::CLOSE_WRITE | WatchMask::MOVED_TO | WatchMask::CREATE,
        );
    }
}

// the image's path, and for a link the file it points to
fn files(path: &Path) -> Vec<PathBuf> {
    let mut files = vec![path.to_path_buf()];

    if let Ok(target) = fs::canonicalize(path)
        && target != path
    {
        files.push(target);
    }

    files
}

/*
 * the image is decoded without holding the service,
 * so the window keeps drawing while a large wallpaper loads
 */
fn reread() {
    let (path, count, modified) = {
        let palette = Palette::read();

        let Some(path) = palette.path.clone() else {
            return;
        };

        (path, palette.count, palette.modified)
    };

    // a link may point into another folder now
    watch_folder(&path);

    let now = modified_time(&path);

    if now.is_none() || now == modified {
        return;
    }

    let colors = quantize(&path, count);

    // a half written file, the next change brings the rest
    if colors.is_empty() {
        return;
    }

    let mut palette = Palette::write();

    // opened again while decoding, the newer path wins
    if palette.path.as_deref() != Some(path.as_path()) {
        return;
    }

    palette.modified = now;
    palette.colors = colors;
}

impl Palette {
    /*
     * reads the image right away, so the first frame already has its
     * colors; 16 is enough for a whole shell's theme
     */
    pub fn open(&mut self, path: &str, count: usize) {
        let path = PathBuf::from(path);

        self.modified = modified_time(&path);
        self.colors = quantize(&path, count);

        watch_folder(&path);

        self.path = Some(path);
        self.count = count;
    }

    // sorted by how much of the image each one covers
    pub fn colors(&self) -> &[Color] {
        &self.colors
    }

    // the color the image shows most
    pub fn dominant(&self) -> Color {
        self.colors.first().copied().unwrap_or(FALLBACK_BACKGROUND)
    }

    // the most vivid color, for highlights and the focused workspace
    pub fn accent(&self) -> Color {
        pick::most_vivid(&self.colors).unwrap_or(FALLBACK_ACCENT)
    }

    // the darkest color, so light text always reads on it
    pub fn background(&self) -> Color {
        pick::darkest(&self.colors).unwrap_or(FALLBACK_BACKGROUND)
    }

    // readable on background, tinted by the image when a palette color is readable
    pub fn foreground(&self) -> Color {
        pick::readable_on(self.background(), &self.colors)
    }

    // white or black, for text on top of accent
    pub fn on_accent(&self) -> Color {
        pick::plain_on(self.accent())
    }

    /*
     * true when the image looks bright overall, for choosing a light theme;
     * judged by lightness as the eye sees it, where 50 is middle grey, since
     * linear brightness calls even a pale pastel picture dark
     */
    pub fn light(&self) -> bool {
        let mut total = 0.0;

        for &color in &self.colors {
            total += pick::lightness(color);
        }

        let average = total / self.colors.len().max(1) as f32;

        average >= 50.0
    }
}

fn quantize(path: &Path, count: usize) -> Vec<Color> {
    /*
     * a wallpaper can be read while it's still being written,
     * and a half written file must not take the shell down with it
     */
    let Some(image) = image::read(path) else {
        return Vec::new();
    };

    let pixels = sample::pixels(&image);

    median_cut::quantize(pixels, count)
}

fn modified_time(path: &Path) -> Option<SystemTime> {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_is_watched_with_the_file_it_points_to() {
        let base = std::env::temp_dir().join(format!("amane-palette-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("pictures")).unwrap();

        let picture = base.join("pictures/wallpaper.png");
        let link = base.join("wallpaper");

        fs::write(&picture, b"").unwrap();
        std::os::unix::fs::symlink(&picture, &link).unwrap();

        let picture = fs::canonicalize(&picture).unwrap();

        assert_eq!(files(&link), vec![link.clone(), picture.clone()]);

        // a plain file is only itself
        assert_eq!(files(&picture), vec![picture.clone()]);

        let _ = fs::remove_dir_all(&base);
    }
}
