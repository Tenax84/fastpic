//! Frissítés-ellenőrzés a GitHub-kiadások alapján: verzió-összehasonlítás, a
//! `releases/latest` válaszának elemzése, napi ütemezés, SHA-256 a letöltés ellenőrzéséhez.
//! A hálózati réteg platformfüggő; itt csak a tiszta logika van.

use std::path::{Path, PathBuf};

pub const LATEST_URL: &str = "https://api.github.com/repos/Tenax84/fastpic/releases/latest";
/// Letölteni csak innen engedünk (a JSON-ban érkező URL-t is ehhez mérjük).
pub const DOWNLOAD_PREFIX: &str = "https://github.com/Tenax84/fastpic/releases/download/";
const DAY_SECS: u64 = 24 * 60 * 60;

/// „v0.2.0”, „0.2”, „0.2.0.1” → összehasonlítható négyes. Minden más (pl. „1.0-beta”) `None`.
pub fn parse_version(s: &str) -> Option<[u32; 4]> {
    let s = s.trim();
    let s = s.strip_prefix(['v', 'V']).unwrap_or(s);
    let mut v = [0u32; 4];
    let mut n = 0;
    for part in s.split('.') {
        if n == 4 || part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        v[n] = part.parse().ok()?;
        n += 1;
    }
    Some(v)
}

pub fn is_newer(latest: &str, current: &str) -> bool {
    matches!((parse_version(latest), parse_version(current)), (Some(l), Some(c)) if l > c)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub sha256: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// „v” nélkül, csak számjegyek és pontok.
    pub version: String,
    /// A kiadás weboldala (hordozható változatnál ezt nyitjuk meg).
    pub page: String,
    /// A `prefix`-szel kezdődő, `.exe` végű fájl, ha van, és van hozzá SHA-256.
    pub installer: Option<Asset>,
}

/// A `releases/latest` JSON-válaszából a nekünk szükséges mezők.
pub fn parse_release(json: &str, installer_prefix: &str) -> Option<Release> {
    let root = Json::parse(json)?;
    if root.get("draft").and_then(Json::as_bool) == Some(true)
        || root.get("prerelease").and_then(Json::as_bool) == Some(true)
    {
        return None;
    }
    let tag = root.get("tag_name")?.as_str()?;
    let v = parse_version(tag)?;
    let n = tag.trim_start_matches(['v', 'V']).split('.').count();
    let version = v[..n].iter().map(u32::to_string).collect::<Vec<_>>().join(".");
    let page = root.get("html_url").and_then(Json::as_str).unwrap_or("").to_string();
    if !page.starts_with("https://github.com/") {
        return None;
    }
    let installer = match root.get("assets") {
        Some(Json::Arr(assets)) => assets.iter().find_map(|a| {
            let name = a.get("name")?.as_str()?;
            let url = a.get("browser_download_url")?.as_str()?;
            if !name.starts_with(installer_prefix) || !name.to_ascii_lowercase().ends_with(".exe") {
                return None;
            }
            if !url.starts_with(DOWNLOAD_PREFIX) || url.contains("..") {
                return None;
            }
            let sha256 = parse_hex32(a.get("digest")?.as_str()?.strip_prefix("sha256:")?)?;
            Some(Asset { name: name.to_string(), url: url.to_string(), sha256 })
        }),
        _ => None,
    };
    Some(Release { version, page, installer })
}

fn parse_hex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// `https://host/út` → (host, „/út”). Csak HTTPS-t fogadunk el.
pub fn split_https_url(url: &str) -> Option<(&str, &str)> {
    let rest = url.strip_prefix("https://")?;
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    (!host.is_empty() && !host.contains(['@', ':'])).then_some((host, path))
}

// ---------------------------------------------------------------------------------------------
// Ütemezés: `<konfig mappa>/FastPic/update.ini`. Külön fájl, mert a `state.ini`-t bezáráskor
// egészében felülírja az utoljára záródó példány.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Schedule {
    /// `check=0` a fájlban: soha nem ellenőriz.
    pub enabled: bool,
    /// Az utolsó ellenőrzés ideje (Unix-másodperc).
    pub last: u64,
}

impl Schedule {
    pub fn parse(s: &str) -> Schedule {
        let mut r = Schedule { enabled: true, last: 0 };
        for line in s.lines() {
            match line.split_once('=').map(|(k, v)| (k.trim(), v.trim())) {
                Some(("check", v)) => r.enabled = v != "0",
                Some(("last", v)) => r.last = v.parse().unwrap_or(0),
                _ => {}
            }
        }
        r
    }

    pub fn serialize(&self) -> String {
        format!("check={}\nlast={}\n", self.enabled as u8, self.last)
    }

    /// Naponta legfeljebb egyszer; visszaállított óránál (a mentett idő a jövőben) újra.
    pub fn due(&self, now: u64) -> bool {
        self.enabled && (now < self.last || now - self.last >= DAY_SECS)
    }
}

pub fn schedule_file(config_dir: Option<PathBuf>) -> Option<PathBuf> {
    Some(config_dir?.join("FastPic").join("update.ini"))
}

/// Ha esedékes az ellenőrzés, még a lekérés előtt beírja az időpontot (így a párhuzamosan
/// futó példányok nem kérdeznek le mind), és `true`-t ad.
pub fn claim_check(path: &Path, now: u64) -> bool {
    let mut s = Schedule::parse(&std::fs::read_to_string(path).unwrap_or_default());
    if !s.due(now) {
        return false;
    }
    s.last = now;
    super::state::write_atomic(path, &s.serialize()).is_ok()
}

// ---------------------------------------------------------------------------------------------
// Minimális JSON-elemző: a GitHub-válaszhoz elég, és nem hoz be függőséget.

#[derive(Debug, PartialEq)]
enum Json {
    Null,
    Bool(bool),
    Num,
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    fn parse(s: &str) -> Option<Json> {
        let mut p = Parser { b: s.as_bytes(), i: 0, depth: 0 };
        let v = p.value()?;
        p.ws();
        (p.i == p.b.len()).then_some(v)
    }

    fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(m) => m.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
    depth: u32,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> bool {
        self.ws();
        if self.b.get(self.i) == Some(&c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn lit(&mut self, s: &str, v: Json) -> Option<Json> {
        self.b[self.i..].starts_with(s.as_bytes()).then(|| {
            self.i += s.len();
            v
        })
    }

    fn value(&mut self) -> Option<Json> {
        self.ws();
        self.depth += 1;
        if self.depth > 64 {
            return None;
        }
        let v = match *self.b.get(self.i)? {
            b'{' => {
                self.i += 1;
                let mut m = Vec::new();
                if !self.eat(b'}') {
                    loop {
                        self.ws();
                        let k = self.string()?;
                        if !self.eat(b':') {
                            return None;
                        }
                        m.push((k, self.value()?));
                        if self.eat(b'}') {
                            break;
                        }
                        if !self.eat(b',') {
                            return None;
                        }
                    }
                }
                Json::Obj(m)
            }
            b'[' => {
                self.i += 1;
                let mut a = Vec::new();
                if !self.eat(b']') {
                    loop {
                        a.push(self.value()?);
                        if self.eat(b']') {
                            break;
                        }
                        if !self.eat(b',') {
                            return None;
                        }
                    }
                }
                Json::Arr(a)
            }
            b'"' => Json::Str(self.string()?),
            b't' => self.lit("true", Json::Bool(true))?,
            b'f' => self.lit("false", Json::Bool(false))?,
            b'n' => self.lit("null", Json::Null)?,
            b'-' | b'0'..=b'9' => {
                let start = self.i;
                while self.i < self.b.len() && matches!(self.b[self.i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
                {
                    self.i += 1;
                }
                std::str::from_utf8(&self.b[start..self.i]).ok()?.parse::<f64>().ok()?;
                Json::Num
            }
            _ => return None,
        };
        self.depth -= 1;
        Some(v)
    }

    fn hex4(&mut self) -> Option<u32> {
        let h = std::str::from_utf8(self.b.get(self.i..self.i + 4)?).ok()?;
        self.i += 4;
        u32::from_str_radix(h, 16).ok()
    }

    fn string(&mut self) -> Option<String> {
        if self.b.get(self.i) != Some(&b'"') {
            return None;
        }
        self.i += 1;
        let mut out = Vec::new();
        loop {
            let c = *self.b.get(self.i)?;
            self.i += 1;
            match c {
                b'"' => return String::from_utf8(out).ok(),
                b'\\' => {
                    let e = *self.b.get(self.i)?;
                    self.i += 1;
                    let ch = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let hi = self.hex4()?;
                            let cp = if (0xD800..0xDC00).contains(&hi) {
                                if self.b.get(self.i..self.i + 2) != Some(b"\\u") {
                                    return None;
                                }
                                self.i += 2;
                                let lo = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&lo) {
                                    return None;
                                }
                                0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                            } else {
                                hi
                            };
                            char::from_u32(cp)?
                        }
                        _ => return None,
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                0..0x20 => return None,
                _ => out.push(c),
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// SHA-256 (FIPS 180-4) a letöltött telepítő ellenőrzéséhez.

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98,
    0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
    0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8,
    0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
    0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819,
    0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
    0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
    0xc67178f2,
];

pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] =
        [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let mut tail = data[data.len() / 64 * 64..].to_vec();
    tail.push(0x80);
    while tail.len() % 64 != 56 {
        tail.push(0);
    }
    tail.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for block in data.chunks_exact(64).chain(tail.chunks_exact(64)) {
        let mut w = [0u32; 64];
        for (i, c) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([c[0], c[1], c[2], c[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 32];
    for (i, x) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&x.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version("v0.2.0"), Some([0, 2, 0, 0]));
        assert_eq!(parse_version("1.10"), Some([1, 10, 0, 0]));
        assert_eq!(parse_version("0.1.0.0"), Some([0, 1, 0, 0]));
        assert_eq!(parse_version("1.0-beta"), None);
        assert_eq!(parse_version("1.2.3.4.5"), None);
        assert_eq!(parse_version(""), None);
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("v0.10.0", "0.9.9"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("v0.1.0", "0.2.0"));
        assert!(!is_newer("nightly", "0.1.0"));
    }

    const DIGEST: &str = "6354b4fe11917e768470a1c95adf60efc5c6bdda6cbd6fc0a03f97dffdad8b25";

    fn sample(tag: &str, digest: &str, url_prefix: &str) -> String {
        format!(
            r#"{{"tag_name":"{tag}","html_url":"https://github.com/Tenax84/fastpic/releases/tag/{tag}",
            "draft":false,"prerelease":false,"name":"FastPic é 😀","body":"a\nb \"q\"",
            "author":{{"id":1,"x":[1,2.5e3,-0.1,null,true]}},
            "assets":[
              {{"name":"fastpic.exe","browser_download_url":"{url_prefix}{tag}/fastpic.exe","digest":"sha256:{digest}"}},
              {{"name":"FastPic-Setup-0.2.0.0.exe","size":4054949,
                "browser_download_url":"{url_prefix}{tag}/FastPic-Setup-0.2.0.0.exe","digest":"sha256:{digest}"}}
            ]}}"#
        )
    }

    #[test]
    fn parses_release() {
        let r = parse_release(&sample("v0.2.0", DIGEST, DOWNLOAD_PREFIX), "FastPic-Setup-").unwrap();
        assert_eq!(r.version, "0.2.0");
        assert_eq!(r.page, "https://github.com/Tenax84/fastpic/releases/tag/v0.2.0");
        let a = r.installer.unwrap();
        assert_eq!(a.name, "FastPic-Setup-0.2.0.0.exe");
        assert_eq!(a.url, format!("{DOWNLOAD_PREFIX}v0.2.0/FastPic-Setup-0.2.0.0.exe"));
        assert_eq!(a.sha256[..2], [0x63, 0x54]);
    }

    #[test]
    fn rejects_bad_releases() {
        // Idegen letöltési hely vagy hiányzó lenyomat: nincs telepítő, csak a kiadás oldala.
        let r = parse_release(&sample("v0.2.0", DIGEST, "https://evil.example/"), "FastPic-Setup-").unwrap();
        assert!(r.installer.is_none());
        let r = parse_release(&sample("v0.2.0", "abc", DOWNLOAD_PREFIX), "FastPic-Setup-").unwrap();
        assert!(r.installer.is_none());
        assert!(parse_release(&sample("latest", DIGEST, DOWNLOAD_PREFIX), "FastPic-Setup-").is_none());
        assert!(parse_release("{\"tag_name\":\"v1.0\"", "FastPic-Setup-").is_none());
        assert!(parse_release("{\"tag_name\":\"v1.0\",\"prerelease\":true}", "x").is_none());
        assert!(parse_release("[]", "x").is_none());
    }

    #[test]
    fn json_strings() {
        let j = Json::parse(r#"{"a":"á😀\/\t","b":[]}"#).unwrap();
        assert_eq!(j.get("a").and_then(Json::as_str), Some("á😀/\t"));
        assert_eq!(j.get("b"), Some(&Json::Arr(vec![])));
        assert!(Json::parse(r#"{"a":"\ud83d"}"#).is_none());
        assert!(Json::parse("{\"a\":1} x").is_none());
        assert!(Json::parse(&"[".repeat(1000)).is_none());
    }

    #[test]
    fn urls() {
        assert_eq!(split_https_url("https://api.github.com/repos/x"), Some(("api.github.com", "/repos/x")));
        assert_eq!(split_https_url("https://github.com"), Some(("github.com", "/")));
        assert_eq!(split_https_url("http://github.com/x"), None);
        assert_eq!(split_https_url("https://a@b.com/x"), None);
    }

    #[test]
    fn schedule() {
        let s = Schedule::parse("");
        assert_eq!(s, Schedule { enabled: true, last: 0 });
        assert!(s.due(1_000_000));
        let s = Schedule::parse("check=1\nlast=1000000\n");
        assert!(!s.due(1_000_000 + DAY_SECS - 1));
        assert!(s.due(1_000_000 + DAY_SECS));
        assert!(s.due(999_999), "visszaállított óra");
        assert!(!Schedule::parse("check=0").due(u64::MAX));
        assert_eq!(Schedule::parse(&s.serialize()), s);
    }

    #[test]
    fn claims_once_per_day() {
        let dir = std::env::temp_dir().join(format!("fastpic-update-test-{}", std::process::id()));
        let file = dir.join("update.ini");
        assert!(claim_check(&file, 5_000_000));
        assert!(!claim_check(&file, 5_000_100));
        assert!(claim_check(&file, 5_000_000 + DAY_SECS));
        std::fs::write(&file, "check=0\n").unwrap();
        assert!(!claim_check(&file, u64::MAX));
        let _ = std::fs::remove_dir_all(dir);
    }

    fn hex(d: [u8; 32]) -> String {
        d.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn sha256_vectors() {
        assert_eq!(hex(sha256(b"")), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(hex(sha256(b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(
            hex(sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        // Blokkhatárok körül (64 és 55 bájt): a kitöltés külön blokkba csúszik.
        assert_eq!(hex(sha256(&[b'a'; 64])), "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb");
        assert_eq!(hex(sha256(&[b'a'; 55])), "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318");
        assert_eq!(
            hex(sha256(&vec![b'a'; 1_000_000])),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }
}
