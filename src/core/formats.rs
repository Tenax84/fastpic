//! Támogatott kiterjesztések és formátumfelismerés fájlfejléc alapján.

use std::path::Path;

/// Minden kiterjesztés, ami bekerül a lapozható listába (kisbetűvel).
/// A HEIC/AVIF Windowson csak telepített WIC-kodekkel nyílik meg, de listázzuk,
/// hogy a felhasználó érthető hibaüzenetet kapjon.
pub const EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "jpe", "jfif", "png", "apng", "bmp", "dib", "gif", "tif", "tiff", "ico", "cur", "webp", "jxl",
    "svg", "svgz", "qoi", "tga", "pnm", "pbm", "pgm", "ppm", "pam", "dds", "hdr", "heic", "heif", "hif", "avif", "jxr",
    "wdp", "hdp",
];

pub fn is_supported(path: &Path) -> bool {
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => EXTENSIONS.iter().any(|e| e.eq_ignore_ascii_case(ext)),
        None => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sniffed {
    Svg,
    Jxl,
    Heif,
    WebP,
    /// Minden más: először a natív dekóder próbálkozik, utána a tartalékok.
    Other,
}

/// A fájl első néhány bájtja (és SVG-nél a kiterjesztés) alapján dönt.
pub fn sniff(path: &Path, head: &[u8]) -> Sniffed {
    // JPEG XL: csupasz kódfolyam vagy ISOBMFF konténer.
    if head.starts_with(&[0xFF, 0x0A])
        || head.starts_with(&[0, 0, 0, 0x0C, b'J', b'X', b'L', b' ', 0x0D, 0x0A, 0x87, 0x0A])
    {
        return Sniffed::Jxl;
    }
    if head.len() >= 12 && &head[4..8] == b"ftyp" {
        let brand = &head[8..12];
        if [b"heic", b"heix", b"hevc", b"hevx", b"heim", b"heis", b"mif1", b"msf1", b"avif", b"avis"]
            .iter()
            .any(|b| brand == *b)
        {
            return Sniffed::Heif;
        }
    }
    if head.len() >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        return Sniffed::WebP;
    }
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if ext.eq_ignore_ascii_case("svg") || ext.eq_ignore_ascii_case("svgz") {
        return Sniffed::Svg;
    }
    // Kiterjesztés nélküli / rossz kiterjesztésű SVG.
    let text = String::from_utf8_lossy(&head[..head.len().min(512)]);
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    if (trimmed.starts_with("<?xml") || trimmed.starts_with("<svg")) && text.contains("<svg") {
        return Sniffed::Svg;
    }
    Sniffed::Other
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions() {
        assert!(is_supported(Path::new("a/b/C.JPG")));
        assert!(is_supported(Path::new("x.webp")));
        assert!(!is_supported(Path::new("x.txt")));
        assert!(!is_supported(Path::new("noext")));
    }

    #[test]
    fn sniffing() {
        assert_eq!(sniff(Path::new("a.bin"), &[0xFF, 0x0A, 1, 2]), Sniffed::Jxl);
        assert_eq!(sniff(Path::new("a.svg"), b"whatever"), Sniffed::Svg);
        assert_eq!(sniff(Path::new("a"), b"<?xml version='1.0'?><svg>"), Sniffed::Svg);
        assert_eq!(sniff(Path::new("a.heic"), b"\0\0\0\x18ftypheic\0\0\0\0"), Sniffed::Heif);
        assert_eq!(sniff(Path::new("a.jpg"), &[0xFF, 0xD8, 0xFF]), Sniffed::Other);
        assert_eq!(sniff(Path::new("a.bin"), b"RIFF\x10\0\0\0WEBPVP8 "), Sniffed::WebP);
    }
}
