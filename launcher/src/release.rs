// What the self-update (update.rs) reads in GitHub's description of the newest
// release, and how it tells whether that release is newer than this build.
//
// It is the launcher's library target, apart from the binary, so that its tests run:
// the binary has no harness to run them with, as its `main` is the C runtime's
// (main.rs). `cargo test -p mw2-launcher --lib` runs these alone.
#![deny(unsafe_code)]

/// The release this build is ("v0.8.2"), cargo xtask's --release-tag, or empty: a build
/// nobody released has nothing to compare with and does not update.
pub const CURRENT: &str = env!("MW2_RELEASE_TAG");
/// Its system, as the release workflow names its archives.
pub const KIND: &str = if cfg!(windows) { "windows" } else { "linux" };

pub fn current() -> &'static str {
    CURRENT
}

pub fn kind() -> &'static str {
    KIND
}

// An archive is mw2-<tag>-<system>, as the workflow names it; until v0.7.x,
// mw2-<tag>-<system>-<online service>.
pub const EXTENSION: &str = if cfg!(windows) { ".zip" } else { ".tar.gz" };

fn archive_end() -> String {
    format!("-{}{EXTENSION}", kind())
}

/// Where a player looks for a release by hand.
pub const PAGE: &str = "https://github.com/PaulCombal/mw2-recompiled/releases";

/// A release a look found.
#[derive(Clone, Default, Debug, PartialEq)]
pub struct Release {
    pub version: String,
    pub archive: String,
    pub sha256: String,
    pub url: String,
    pub size: u64,
}

/// What a look found.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Check {
    Newer,
    UpToDate,
    Failed,
}

// The value of the first `"key":` at or after `from` in a JSON text, as written: a
// string without its quotes, or a number. The description's strings here -- a tag, a
// file name, a digest, an address -- hold nothing escaped.
fn value<'a>(json: &'a str, key: &str, from: usize) -> &'a str {
    let quoted = format!("\"{key}\"");
    let Some(found) = json.get(from..).and_then(|rest| rest.find(&quoted)) else { return "" };
    let after = from + found + quoted.len();
    let Some(colon) = json[after..].find(':') else { return "" };
    let start = after + colon + 1;
    let Some(skip) = json[start..].find(|c: char| !matches!(c, ' ' | '\t' | '\r' | '\n')) else { return "" };
    let at = start + skip;
    let rest = &json[at..];
    if let Some(string) = rest.strip_prefix('"') {
        return match string.find('"') {
            Some(end) => &string[..end],
            None => "",
        };
    }
    let end = rest.find([',', '}', '\r', '\n', ' ']).unwrap_or(rest.len());
    &rest[..end]
}

// "v1.2.3" as numbers; a tag that is not one compares as older than any.
fn numbers(tag: &str) -> Vec<u64> {
    let digits = tag.strip_prefix('v').unwrap_or(tag);
    if digits.is_empty() {
        return Vec::new();
    }
    let mut numbers = Vec::new();
    // Each part is digits, the parts joined by dots; a dot at the end ends it,
    // with nothing after it.
    let mut parts = digits.split('.').peekable();
    while let Some(part) = parts.next() {
        if part.is_empty() {
            if parts.peek().is_none() && !numbers.is_empty() {
                break;
            }
            return Vec::new();
        }
        if !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return Vec::new();
        }
        numbers.push(part.bytes().fold(0u64, |number, digit| number.saturating_mul(10).saturating_add(u64::from(digit - b'0'))));
    }
    numbers
}

// The leading digits of a number as written, as strtoull reads them.
fn unsigned(text: &str) -> u64 {
    text.bytes()
        .take_while(u8::is_ascii_digit)
        .fold(0u64, |number, digit| number.saturating_mul(10).saturating_add(u64::from(digit - b'0')))
}

/// The description's facts for this build's system, its archive's if it has one.
pub fn read_description(json: &str) -> Release {
    let mut release = Release { version: value(json, "tag_name", 0).to_string(), ..Release::default() };
    // An archive's entry gives its name, then its size, digest and address.
    let end = archive_end();
    let mut from = 0;
    while let Some(found) = json[from..].find("\"name\"") {
        let at = from + found;
        from = at + 1;
        let name = value(json, "name", at);
        if name.len() <= end.len() || !name.ends_with(&end) {
            continue;
        }
        let digest = value(json, "digest", at);
        release.archive = name.to_string();
        release.size = unsigned(value(json, "size", at));
        release.sha256 = digest.strip_prefix("sha256:").unwrap_or("").to_string();
        release.url = value(json, "browser_download_url", at).to_string();
        break;
    }
    release
}

/// Whether the release a description gives is newer than this build, and what to say
/// when it cannot be had.
pub fn judge(release: &Release) -> Result<Check, String> {
    let newest = numbers(&release.version);
    if newest.is_empty() {
        return Err("The newest version could not be looked up: its description is not one.".into());
    }
    if newest <= numbers(current()) {
        return Ok(Check::UpToDate);
    }
    if release.archive.is_empty() || release.sha256.is_empty() || release.url.is_empty() {
        return Err(format!("Version {} is out, without a build for this system. Look on\n{PAGE}", release.version));
    }
    Ok(Check::Newer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_as_written() {
        let json = r#"{"tag_name": "v0.9.1", "assets": [{"name":"a","size": 12,"x":"y"}]}"#;
        assert_eq!(value(json, "tag_name", 0), "v0.9.1");
        assert_eq!(value(json, "size", 0), "12");
        assert_eq!(value(json, "missing", 0), "");
        assert_eq!(value(json, "name", json.len()), "");
        assert_eq!(value(r#"{"a": "#, "a", 0), "");
        assert_eq!(value(r#"{"a": "open"#, "a", 0), "");
        assert_eq!(value(r#"{"a":5"#, "a", 0), "5");
    }

    #[test]
    fn tags_as_numbers() {
        assert_eq!(numbers("v1.2.3"), [1, 2, 3]);
        assert_eq!(numbers("0.10"), [0, 10]);
        assert_eq!(numbers("v1.2."), [1, 2]);
        assert!(numbers("").is_empty());
        assert!(numbers("v").is_empty());
        assert!(numbers("v1..2").is_empty());
        assert!(numbers("v1.2-rc1").is_empty());
        assert!(numbers(".1").is_empty());
        assert!(numbers("v99999999999999999999999.1").len() == 2);
        assert!(numbers("v0.10.0") > numbers("v0.9.9"));
        assert!(numbers("v1.0") > numbers(""));
    }

    #[test]
    fn this_systems_archive() {
        let kind = kind();
        let json = format!(
            r#"{{"tag_name":"v9.0.0","assets":[
            {{"name":"mw2-v9.0.0-{kind}-steam{EXTENSION}","size":5,"digest":"sha256:bad","browser_download_url":"https://x/old"}},
            {{"name":"mw2-v9.0.0-{kind}{EXTENSION}","size":123456789,"digest":"sha256:abc","browser_download_url":"https://x/new"}}]}}"#
        );
        let release = read_description(&json);
        assert_eq!(release.version, "v9.0.0");
        assert_eq!(release.archive, format!("mw2-v9.0.0-{kind}{EXTENSION}"));
        assert_eq!(release.size, 123456789);
        assert_eq!(release.sha256, "abc");
        assert_eq!(release.url, "https://x/new");
        assert_eq!(judge(&release), Ok(Check::Newer));
        let none = read_description(r#"{"tag_name":"v9.0.0","assets":[]}"#);
        assert_eq!(
            judge(&none),
            Err("Version v9.0.0 is out, without a build for this system. Look on\nhttps://github.com/PaulCombal/mw2-recompiled/releases".into())
        );
        assert!(judge(&read_description("not json")).is_err());
    }
}
