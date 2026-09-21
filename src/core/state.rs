//! Ablakállapot mentése és betöltése egy apró szövegfájlba, atomi írással.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowState {
    /// A nem maximalizált ablak téglalapja (Windowson workspace-koordinátában).
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub maximized: bool,
    /// Az utolsó kliensterület mérete: ebből számoljuk a dekódolási célméretet még
    /// az ablak létrehozása előtt.
    pub client_w: u32,
    pub client_h: u32,
}

impl WindowState {
    pub fn parse(s: &str) -> Option<WindowState> {
        let mut rect = None;
        let mut maximized = false;
        let mut client = None;
        for line in s.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let nums: Vec<i64> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
            match (k.trim(), nums.as_slice()) {
                ("rect", &[l, t, r, b]) => rect = Some((l as i32, t as i32, r as i32, b as i32)),
                ("maximized", &[m]) => maximized = m != 0,
                ("client", &[w, h]) if w > 0 && h > 0 => client = Some((w as u32, h as u32)),
                _ => {}
            }
        }
        let (left, top, right, bottom) = rect?;
        if right - left < 50 || bottom - top < 50 {
            return None;
        }
        let (client_w, client_h) = client.unwrap_or(((right - left) as u32, (bottom - top) as u32));
        Some(WindowState { left, top, right, bottom, maximized, client_w, client_h })
    }

    pub fn serialize(&self) -> String {
        format!(
            "rect={},{},{},{}\nmaximized={}\nclient={},{}\n",
            self.left, self.top, self.right, self.bottom, self.maximized as u8, self.client_w, self.client_h
        )
    }

    pub fn load(path: &Path) -> Option<WindowState> {
        WindowState::parse(&std::fs::read_to_string(path).ok()?)
    }

    /// Ideiglenes fájlba ír, majd átnevezi, így párhuzamosan záródó példányok sem
    /// hagyhatnak félig írt fájlt maguk után. Az utoljára záródó nyer.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
        std::fs::write(&tmp, self.serialize())?;
        std::fs::rename(&tmp, path).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
    }
}

/// `<konfig mappa>/FastPic/state.ini`
pub fn state_file(config_dir: Option<PathBuf>) -> Option<PathBuf> {
    Some(config_dir?.join("FastPic").join("state.ini"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let s = WindowState {
            left: -1900,
            top: 10,
            right: -100,
            bottom: 900,
            maximized: true,
            client_w: 1784,
            client_h: 851,
        };
        assert_eq!(WindowState::parse(&s.serialize()), Some(s));
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(WindowState::parse(""), None);
        assert_eq!(WindowState::parse("rect=1,2,3"), None);
        assert_eq!(WindowState::parse("rect=0,0,10,10"), None);
    }

    #[test]
    fn save_and_load() {
        let dir = std::env::temp_dir().join(format!("fastpic-test-{}", std::process::id()));
        let file = dir.join("state.ini");
        let s =
            WindowState { left: 1, top: 2, right: 801, bottom: 602, maximized: false, client_w: 784, client_h: 561 };
        s.save(&file).unwrap();
        assert_eq!(WindowState::load(&file), Some(s));
        let _ = std::fs::remove_dir_all(dir);
    }
}
