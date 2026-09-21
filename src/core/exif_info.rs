//! Az infópanel tartalma: fájladatok és a fontosabb EXIF mezők.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use exif::{In, Tag, Value};

pub type InfoLines = Vec<(String, String)>;

pub fn read(path: &Path, dims: Option<(u32, u32)>, modified: Option<String>) -> InfoLines {
    let mut out: InfoLines = Vec::new();
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    out.push(("Fájl".into(), name));
    if let Some((w, h)) = dims {
        let mp = w as f64 * h as f64 / 1e6;
        out.push(("Méret".into(), format!("{w} × {h} px ({mp:.1} MP)")));
    }
    if let Ok(md) = std::fs::metadata(path) {
        out.push(("Fájlméret".into(), human_size(md.len())));
    }
    if let Some(m) = modified {
        out.push(("Módosítva".into(), m));
    }

    let Some(exif) =
        File::open(path).ok().and_then(|f| exif::Reader::new().read_from_container(&mut BufReader::new(f)).ok())
    else {
        return out;
    };

    let text = |tag: Tag| -> Option<String> {
        let f = exif.get_field(tag, In::PRIMARY)?;
        let s = f.display_value().with_unit(&exif).to_string();
        let s = s.trim().trim_matches('"').trim().to_string();
        if s.is_empty() || s == "unknown" { None } else { Some(s) }
    };

    if let Some(v) = text(Tag::DateTimeOriginal).or_else(|| text(Tag::DateTime)) {
        out.push(("Készült".into(), v));
    }
    let camera = match (text(Tag::Make), text(Tag::Model)) {
        (Some(make), Some(model)) if model.starts_with(&make) => Some(model),
        (Some(make), Some(model)) => Some(format!("{make} {model}")),
        (a, b) => a.or(b),
    };
    if let Some(v) = camera {
        out.push(("Fényképezőgép".into(), v));
    }
    if let Some(v) = text(Tag::LensModel) {
        out.push(("Objektív".into(), v));
    }
    let exposure: Vec<String> =
        [Tag::ExposureTime, Tag::FNumber, Tag::FocalLength].into_iter().filter_map(text).collect();
    if !exposure.is_empty() {
        out.push(("Expozíció".into(), exposure.join("   ")));
    }
    if let Some(v) = text(Tag::PhotographicSensitivity) {
        out.push(("ISO".into(), v));
    }
    if let Some((lat, lon)) = gps(&exif) {
        out.push(("GPS".into(), format!("{lat:.6}, {lon:.6}")));
    }
    out
}

fn gps(exif: &exif::Exif) -> Option<(f64, f64)> {
    let coord = |tag: Tag, ref_tag: Tag, neg: &str| -> Option<f64> {
        let Value::Rational(v) = &exif.get_field(tag, In::PRIMARY)?.value else { return None };
        if v.len() < 3 {
            return None;
        }
        let deg = v[0].to_f64() + v[1].to_f64() / 60.0 + v[2].to_f64() / 3600.0;
        let r = exif.get_field(ref_tag, In::PRIMARY)?.display_value().to_string();
        Some(if r.contains(neg) { -deg } else { deg })
    };
    let lat = coord(Tag::GPSLatitude, Tag::GPSLatitudeRef, "S")?;
    let lon = coord(Tag::GPSLongitude, Tag::GPSLongitudeRef, "W")?;
    if lat.is_finite() && lon.is_finite() { Some((lat, lon)) } else { None }
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{bytes} B") } else { format!("{v:.1} {}", UNITS[i]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2.0 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
    }
}
