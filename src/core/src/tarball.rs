//! A repository tarball, as GitHub's codeload serves it (`.tar.gz`), read into its files: gzip
//! inflated with miniz_oxide, then the ustar entries walked. Only regular files are kept, paths
//! relative to the archive's one top folder. Pure: no I/O. Symlinks, devices and anything
//! outside the top folder are skipped, and a `..` segment is refused.

use miniz_oxide::inflate::decompress_to_vec_with_limit;

/// The largest archive accepted once inflated.
pub const MAX_INFLATED: usize = 256 * 1024 * 1024;

/// One regular file: its path below the top folder, and its bytes.
pub type File = (String, Vec<u8>);

/// A read archive.
#[derive(Debug, Default)]
pub struct Archive {
    /// The commit GitHub names in the archive's global header (`comment=`), if any.
    pub commit: Option<String>,
    /// The newest entry time, in Unix seconds: GitHub sets every entry to the commit's time.
    pub mtime: u64,
    /// The regular files, in archive order.
    pub files: Vec<File>,
}

/// The raw deflate stream inside a gzip member, after its header.
fn gzip_body(gz: &[u8]) -> Result<&[u8], String> {
    if gz.len() < 18 || gz[0] != 0x1f || gz[1] != 0x8b || gz[2] != 8 {
        return Err("the archive is not gzip".into());
    }
    let flags = gz[3];
    let mut at = 10usize;
    if flags & 0x04 != 0 {
        let xlen = u16::from_le_bytes([gz[at], gz[at + 1]]) as usize;
        at += 2 + xlen;
    }
    for bit in [0x08u8, 0x10] {
        if flags & bit != 0 {
            let end = gz[at..]
                .iter()
                .position(|b| *b == 0)
                .ok_or("the gzip header is cut short")?;
            at += end + 1;
        }
    }
    if flags & 0x02 != 0 {
        at += 2;
    }
    gz.get(at..gz.len().saturating_sub(8))
        .ok_or_else(|| "the gzip header is cut short".into())
}

/// A NUL-terminated field.
fn field(b: &[u8]) -> &[u8] {
    let end = b.iter().position(|c| *c == 0).unwrap_or(b.len());
    &b[..end]
}

/// An octal number field.
fn octal(b: &[u8]) -> Result<usize, String> {
    let text = std::str::from_utf8(field(b)).map_err(|_| "a tar size is not text")?;
    let text = text.trim();
    if text.is_empty() {
        return Ok(0);
    }
    usize::from_str_radix(text, 8).map_err(|_| "a tar size is not octal".into())
}

/// The files in a `.tar.gz`, with the top folder stripped.
pub fn read(gz: &[u8]) -> Result<Archive, String> {
    let tar = decompress_to_vec_with_limit(gzip_body(gz)?, MAX_INFLATED)
        .map_err(|e| format!("the archive does not inflate: {e:?}"))?;
    let mut out = Archive::default();
    let mut long_name: Option<String> = None;
    let mut pax_path: Option<String> = None;
    let mut at = 0usize;
    while at + 512 <= tar.len() {
        let h = &tar[at..at + 512];
        if h.iter().all(|b| *b == 0) {
            break;
        }
        let size = octal(&h[124..136])?;
        out.mtime = out.mtime.max(octal(&h[136..148])? as u64);
        let kind = h[156];
        let body_at = at + 512;
        let body = tar
            .get(body_at..body_at + size)
            .ok_or("a tar entry is cut short")?;
        at = body_at + size.div_ceil(512) * 512;
        match kind {
            b'L' => {
                long_name = Some(String::from_utf8_lossy(field(body)).into_owned());
                continue;
            }
            b'x' => {
                pax_path = pax(body, "path");
                continue;
            }
            b'g' => {
                if out.commit.is_none() {
                    out.commit = pax(body, "comment");
                }
                continue;
            }
            _ => {}
        }
        let name = match (pax_path.take(), long_name.take()) {
            (Some(p), _) | (None, Some(p)) => p,
            (None, None) => {
                let prefix = String::from_utf8_lossy(field(&h[345..500])).into_owned();
                let name = String::from_utf8_lossy(field(&h[0..100])).into_owned();
                if prefix.is_empty() {
                    name
                } else {
                    format!("{prefix}/{name}")
                }
            }
        };
        if kind != b'0' && kind != 0 {
            continue;
        }
        let Some((_, rest)) = name.split_once('/') else {
            continue;
        };
        if rest.is_empty() {
            continue;
        }
        if rest.split('/').any(|s| s == ".." || s == ".") {
            return Err(format!("the archive has an unsafe path: {name}"));
        }
        out.files.push((rest.to_string(), body.to_vec()));
    }
    Ok(out)
}

/// The value of `key` in a pax extended header.
fn pax(body: &[u8], key: &str) -> Option<String> {
    let text = String::from_utf8_lossy(body);
    text.lines().find_map(|line| {
        let (_, kv) = line.split_once(' ')?;
        let (k, v) = kv.split_once('=')?;
        (k == key).then(|| v.to_string())
    })
}

/// The calendar year of a Unix time, in UTC.
pub fn year_of(unix_secs: u64) -> u16 {
    // Howard Hinnant's days-to-civil.
    let z = (unix_secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    year as u16
}
