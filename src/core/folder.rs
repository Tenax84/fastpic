//! A megnyitott kép mappájának listája és a lapozás.

use std::cmp::Ordering;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use super::formats;

/// A mappa támogatott képei, a platform fájlkezelőjének rendezésével.
pub fn list(dir: &Path, cmp: &dyn Fn(&OsStr, &OsStr) -> Ordering) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| !t.is_dir()).unwrap_or(false))
        .map(|e| e.path())
        .filter(|p| formats::is_supported(p))
        .collect();
    files.sort_by(|a, b| cmp(a.file_name().unwrap_or_default(), b.file_name().unwrap_or_default()));
    files
}

fn same_path(a: &Path, b: &Path) -> bool {
    // Windowson és macOS-en (alapból) a fájlrendszer kis-nagybetű érzéketlen.
    a.as_os_str().len() == b.as_os_str().len()
        && a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
}

/// Lapozható lista, az aktuális elem indexével.
#[derive(Default)]
pub struct Nav {
    files: Vec<PathBuf>,
    index: usize,
    current: PathBuf,
}

impl Nav {
    pub fn new(current: PathBuf) -> Nav {
        Nav { files: Vec::new(), index: 0, current }
    }

    pub fn current(&self) -> &Path {
        &self.current
    }

    pub fn is_empty(&self) -> bool {
        self.current.as_os_str().is_empty()
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// 1-től számozott pozíció, ha a lista már megvan.
    pub fn position(&self) -> Option<(usize, usize)> {
        if self.files.is_empty() { None } else { Some((self.index + 1, self.files.len())) }
    }

    /// Új lista érkezett (első betöltés vagy frissítés). Az aktuális képnél maradunk;
    /// ha az már nincs a mappában, a helyére kerülő elem lesz az aktuális.
    pub fn set_files(&mut self, files: Vec<PathBuf>, cmp: &dyn Fn(&OsStr, &OsStr) -> Ordering) {
        if let Some(i) = files.iter().position(|p| same_path(p, &self.current)) {
            // A lista szerinti írásmódot vesszük át, hogy a lapozás során egyezzenek az útvonalak.
            self.current = files[i].clone();
            self.index = i;
            self.files = files;
            return;
        }
        // Az aktuális kép nincs a listában (pl. nem támogatott kiterjesztés, vagy törölték):
        // beszúrjuk a rendezés szerinti helyére, hogy a lapozás onnan folytatódjon.
        let name = self.current.file_name().unwrap_or_default().to_owned();
        let i = files
            .iter()
            .position(|p| cmp(p.file_name().unwrap_or_default(), &name) == Ordering::Greater)
            .unwrap_or(files.len());
        self.files = files;
        self.files.insert(i, self.current.clone());
        self.index = i;
    }

    pub fn peek(&self, delta: isize) -> Option<&Path> {
        let n = self.files.len();
        if n < 2 {
            return None;
        }
        let i = (self.index as isize + delta).rem_euclid(n as isize) as usize;
        Some(&self.files[i])
    }

    /// Lép `delta`-t, körbefordulva. Igaz, ha változott az aktuális kép.
    pub fn step(&mut self, delta: isize) -> bool {
        let n = self.files.len();
        if n < 2 {
            return false;
        }
        self.index = (self.index as isize + delta).rem_euclid(n as isize) as usize;
        self.current = self.files[self.index].clone();
        true
    }

    pub fn go_to(&mut self, index: usize) -> bool {
        if index >= self.files.len() || index == self.index {
            return false;
        }
        self.index = index;
        self.current = self.files[index].clone();
        true
    }

    pub fn go_last(&mut self) -> bool {
        let n = self.files.len();
        n > 0 && self.go_to(n - 1)
    }

    /// Eltávolítja az aktuális elemet (törölték vagy eltűnt), és a következőre lép.
    /// Hamis, ha nem maradt kép.
    pub fn remove_current(&mut self) -> bool {
        if self.files.is_empty() {
            self.current = PathBuf::new();
            return false;
        }
        self.files.remove(self.index);
        if self.files.is_empty() {
            self.current = PathBuf::new();
            return false;
        }
        if self.index >= self.files.len() {
            self.index = 0;
        }
        self.current = self.files[self.index].clone();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmp(a: &OsStr, b: &OsStr) -> Ordering {
        a.cmp(b)
    }

    fn nav_with(names: &[&str], cur: &str) -> Nav {
        let mut n = Nav::new(PathBuf::from(cur));
        n.set_files(names.iter().map(PathBuf::from).collect(), &cmp);
        n
    }

    #[test]
    fn stepping_wraps() {
        let mut n = nav_with(&["a.jpg", "b.jpg", "c.jpg"], "B.JPG");
        assert_eq!(n.position(), Some((2, 3)));
        assert!(n.step(1));
        assert_eq!(n.current(), Path::new("c.jpg"));
        assert!(n.step(1));
        assert_eq!(n.current(), Path::new("a.jpg"));
        assert!(n.step(-1));
        assert_eq!(n.current(), Path::new("c.jpg"));
        assert_eq!(n.peek(1), Some(Path::new("a.jpg")));
    }

    #[test]
    fn missing_current_is_inserted() {
        let n = nav_with(&["a.jpg", "c.jpg"], "b.xyz");
        assert_eq!(n.position(), Some((2, 3)));
        assert_eq!(n.peek(1), Some(Path::new("c.jpg")));
    }

    #[test]
    fn remove() {
        let mut n = nav_with(&["a.jpg", "b.jpg"], "b.jpg");
        assert!(n.remove_current());
        assert_eq!(n.current(), Path::new("a.jpg"));
        assert!(!n.remove_current());
        assert!(n.is_empty());
    }

    #[test]
    fn single_file_does_not_step() {
        let mut n = nav_with(&["a.jpg"], "a.jpg");
        assert!(!n.step(1));
        assert_eq!(n.peek(1), None);
    }
}
