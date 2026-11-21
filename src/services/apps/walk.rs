use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

// folders already walked, by device and inode, so a link back up isn't followed round and round
type Walked = HashSet<(u64, u64)>;

fn first_visit(dir: &Path, walked: &mut Walked) -> bool {
    fs::metadata(dir).is_ok_and(|metadata| walked.insert((metadata.dev(), metadata.ino())))
}

// every file under the dir, in sorted order so the same file wins every scan
pub fn files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();

    add(dir, &mut found, &mut Walked::new());

    found.sort();

    found
}

fn add(dir: &Path, found: &mut Vec<PathBuf>, walked: &mut Walked) {
    if !first_visit(dir, walked) {
        return;
    }

    // a missing dir just has no files
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();

        // follows links, which is how nix puts files into the profile dirs
        if path.is_dir() {
            add(&path, found, walked);
        } else {
            found.push(path);
        }
    }
}

// the dir and every dir under it, following links like files() does
pub fn folders(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();

    if dir.is_dir() {
        add_folders(dir, &mut found, &mut Walked::new());
    }

    found
}

fn add_folders(dir: &Path, found: &mut Vec<PathBuf>, walked: &mut Walked) {
    if !first_visit(dir, walked) {
        return;
    }

    found.push(dir.to_path_buf());

    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();

        if path.is_dir() {
            add_folders(&path, found, walked);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    #[test]
    fn a_link_back_up_is_walked_once() {
        let base = std::env::temp_dir().join(format!("amane-walk-{}", std::process::id()));

        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("kde")).unwrap();
        fs::write(base.join("kde/editor.desktop"), b"").unwrap();
        symlink(&base, base.join("kde/loop")).unwrap();

        assert_eq!(files(&base), vec![base.join("kde/editor.desktop")]);
        assert_eq!(folders(&base), vec![base.clone(), base.join("kde")]);

        let _ = fs::remove_dir_all(&base);
    }
}
