use std::fs;
use std::path::{Path, PathBuf};

// every file under the dir, in sorted order so the same file wins every scan
pub fn files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();

    add(dir, &mut found);

    found.sort();

    found
}

fn add(dir: &Path, found: &mut Vec<PathBuf>) {
    // a missing dir just has no files
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();

        // follows links, which is how nix puts files into the profile dirs
        if path.is_dir() {
            add(&path, found);
        } else {
            found.push(path);
        }
    }
}

// the dir and every dir under it, following links like files() does
pub fn folders(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();

    if dir.is_dir() {
        add_folders(dir, &mut found);
    }

    found
}

fn add_folders(dir: &Path, found: &mut Vec<PathBuf>) {
    found.push(dir.to_path_buf());

    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();

        if path.is_dir() {
            add_folders(&path, found);
        }
    }
}
