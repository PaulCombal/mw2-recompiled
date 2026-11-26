// A game's log with what says who the player is taken out, for a bug report, which is
// public (report.rs): their name, the addresses and numbers of their machine, their
// accounts and the people they played with, and the folders the game and their home are
// in. And the two addresses a report opens, written as a page and a file browser take
// them.
//
// The patterns were C++'s std::regex, in its ECMAScript grammar; each is matched here by
// hand as that grammar matches it -- the leftmost match first, the next one searched
// from the end of the last, \b a change between a word character ([A-Za-z0-9_]) and any
// other or an end, \d an ASCII digit -- over the log's bytes, whatever they are.
// tools/launcher_diff.py holds them to std::regex.
#![deny(unsafe_code)]

fn word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

// \b at `at`: a word character on one side and not on the other.
fn boundary(text: &[u8], at: usize) -> bool {
    let before = at > 0 && word(text[at - 1]);
    let after = at < text.len() && word(text[at]);
    before != after
}

fn find(text: &[u8], what: &[u8], from: usize) -> Option<usize> {
    if what.is_empty() {
        return Some(from.min(text.len()));
    }
    text.get(from..)?.windows(what.len()).position(|window| window == what).map(|at| at + from)
}

/// Every `what` in `text` replaced with `with`, left to right.
pub fn replace_all(text: &mut Vec<u8>, what: &[u8], with: &[u8]) {
    if what.is_empty() {
        return;
    }
    let mut at = 0;
    while let Some(found) = find(text, what, at) {
        text.splice(found..found + what.len(), with.iter().copied());
        at = found + with.len();
    }
}

// Each match of a pattern -- `matched(text, at)` the length of the one starting at `at`,
// or None -- replaced with what `with` makes of it, as std::regex_replace does.
fn replace_matches(
    text: &[u8],
    matched: impl Fn(&[u8], usize) -> Option<usize>,
    with: impl Fn(&[u8]) -> Vec<u8>,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    let (mut at, mut copied) = (0, 0);
    while at < text.len() {
        match matched(text, at) {
            Some(length) => {
                out.extend_from_slice(&text[copied..at]);
                out.extend(with(&text[at..at + length]));
                at += length;
                copied = at;
            }
            None => at += 1,
        }
    }
    out.extend_from_slice(&text[copied..]);
    out
}

fn digits(text: &[u8], at: usize) -> usize {
    text[at.min(text.len())..].iter().take_while(|byte| byte.is_ascii_digit()).count()
}

// \b\d{1,3}(\.\d{1,3}){3}\b. A run of digits can only end where a dot follows it, so
// each \d{1,3} is a whole run of one to three digits: none of the backtracking the
// grammar allows finds another way.
fn address(text: &[u8], start: usize) -> Option<usize> {
    if !boundary(text, start) {
        return None;
    }
    let mut at = start;
    for part in 0..4 {
        if part > 0 {
            if text.get(at) != Some(&b'.') {
                return None;
            }
            at += 1;
        }
        let run = digits(text, at);
        if !(1..=3).contains(&run) {
            return None;
        }
        at += run;
    }
    boundary(text, at).then_some(at - start)
}

fn hex16(text: &[u8], at: usize) -> bool {
    text.get(at..at + 16).is_some_and(|run| run.iter().all(u8::is_ascii_hexdigit))
}

// (u:|mpdata_|user[/\\])[0-9a-fA-F]{16}, and the length of what the group took.
fn account(text: &[u8], at: usize) -> Option<(usize, usize)> {
    let rest = &text[at..];
    let lead = if rest.starts_with(b"u:") {
        2
    } else if rest.starts_with(b"mpdata_") {
        7
    } else if rest.starts_with(b"user/") || rest.starts_with(b"user\\") {
        5
    } else {
        return None;
    };
    hex16(text, at + lead).then_some((lead + 16, lead))
}

// \b[0-9a-fA-F]{16}\b
fn number(text: &[u8], at: usize) -> Option<usize> {
    (boundary(text, at) && hex16(text, at) && boundary(text, at + 16)).then_some(16)
}

// "connect [^"]*"?
fn connect(text: &[u8], at: usize) -> Option<usize> {
    if !text[at..].starts_with(b"\"connect ") {
        return None;
    }
    let end = at + 9 + text[at + 9..].iter().take_while(|&&byte| byte != b'"').count();
    Some(if end < text.len() { end + 1 - at } else { end - at })
}

// as "([^"]*)": the first such, its quotes' insides.
fn name(line: &[u8]) -> Option<&[u8]> {
    let at = find(line, b"as \"", 0)? + 4;
    let end = find(line, b"\"", at)?;
    Some(&line[at..end])
}

// sscanf's "%u.%u" of an address the pattern found: its first two numbers.
fn first_two(address: &[u8]) -> (u32, u32) {
    let mut parts =
        address.split(|&byte| byte == b'.').map(|part| part.iter().fold(0u32, |n, &d| n * 10 + u32::from(d - b'0')));
    (parts.next().unwrap_or(0), parts.next().unwrap_or(0))
}

// An address inside a home or office network says which kind of connection it is on and
// nothing about whose: every router hands out the same ones. Its first half stays, since
// that is what tells a real connection from one a virtual machine or a VPN added. Any
// other address goes entirely.
fn addresses(line: &[u8]) -> Vec<u8> {
    replace_matches(line, address, |found| {
        let (a, b) = first_two(found);
        let kept = if a == 10 || a == 127 {
            format!("{a}.x.x.x")
        } else if (a == 192 && b == 168) || (a == 172 && (16..=31).contains(&b)) || (a == 169 && b == 254) {
            format!("{a}.{b}.x.x")
        } else {
            "<address>".into()
        };
        kept.into_bytes()
    })
}

/// The folders taken out of a log: where the game is, both ways it can be spelt (the
/// system's own separators and forward slashes), and the player's home, both ways.
pub struct Folders {
    pub here: Vec<Vec<u8>>,
    pub home: Vec<Vec<u8>>,
}

/// The log without what says who the player is, a line at a time, each ended by "\n".
pub fn scrub(log: &[u8], folders: &Folders) -> Vec<u8> {
    let mut lines: Vec<&[u8]> = log.split(|&byte| byte == b'\n').collect();
    if log.is_empty() || log.last() == Some(&b'\n') {
        lines.pop();
    }
    let mut out = Vec::with_capacity(log.len());
    let mut player: Vec<u8> = Vec::new();
    for line in lines {
        let mut line = line.strip_suffix(b"\r").unwrap_or(line).to_vec();
        let online = find(&line, b"] online:", 0).is_some();
        if online && let Some(found) = name(&line) {
            player = found.to_vec();
        }
        if online {
            replace_all(&mut line, &player.clone(), b"<player>");
            line = replace_matches(&line, connect, |_| b"\"connect <removed>\"".to_vec());
            line = replace_matches(&line, number, |_| b"<id>".to_vec());
            // Whoever else is named: the other end of a join or an invitation.
            for verb in [&b" lan joins "[..], b" lan invites ", b" lan accepts "] {
                if let Some(at) = find(&line, verb, 0) {
                    line.truncate(at + verb.len());
                    line.extend_from_slice(b"<removed>");
                }
            }
            if let Some(at) = find(&line, b" invites you", 0) {
                // (where "online: " is not there, the C++ took the line from its start)
                let head = find(&line, b"online: ", 0).map_or(7, |at| at + 8);
                let mut kept = line[..head.min(line.len())].to_vec();
                kept.extend_from_slice(b"<removed>");
                kept.extend_from_slice(&line[at..]);
                line = kept;
            }
        }
        // The graphics line names the device and its driver, nothing of the player's, and
        // a driver's version can be four short numbers between dots: AMD's on Windows read
        // as an address and went (issue #50).
        if find(&line, b"] report: graphics ", 0).is_none() {
            line = addresses(&line);
        }
        line = replace_matches(
            &line,
            |text, at| account(text, at).map(|(length, _)| length),
            |found| {
                let lead = account(found, 0).map_or(0, |(_, lead)| lead);
                let mut kept = found[..lead].to_vec();
                kept.extend_from_slice(b"<id>");
                kept
            },
        );
        for (folders, with) in [(&folders.here, &b"<game folder>"[..]), (&folders.home, b"<home>")] {
            for folder in folders {
                replace_all(&mut line, folder, with);
            }
        }
        out.extend_from_slice(&line);
        out.push(b'\n');
    }
    out
}

/// The text after the first `marker`, to the end of its line.
pub fn first_line<'a>(text: &'a [u8], marker: &[u8]) -> &'a [u8] {
    let Some(at) = find(text, marker, 0) else { return &[] };
    let from = at + marker.len();
    let end = find(text, b"\n", from).unwrap_or(text.len());
    &text[from..end]
}

/// How the run performed, as the game summed it up: its "report:" lines but the graphics
/// one, a line each after "- ".
pub fn performance(log: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut lines: Vec<&[u8]> = log.split(|&byte| byte == b'\n').collect();
    if log.is_empty() || log.last() == Some(&b'\n') {
        lines.pop();
    }
    for line in lines {
        if let Some(at) = find(line, b"] report: ", 0)
            && find(line, b"report: graphics", 0).is_none()
        {
            out.extend_from_slice(b"- ");
            out.extend_from_slice(&line[at + 10..]);
            out.push(b'\n');
        }
    }
    out
}

/// Text for a page's address: letters, digits and -_.~ as they are, every other byte as
/// %XX.
pub fn encoded(text: &[u8]) -> String {
    let mut out = String::with_capacity(text.len());
    for &c in text {
        if c.is_ascii_alphanumeric() || b"-_.~".contains(&c) {
            out.push(c as char);
        } else {
            out.push_str(&format!("%{c:02X}"));
        }
    }
    out
}

/// A folder, written with forward slashes, as a file:// address.
pub fn file_address(generic: &[u8]) -> String {
    let mut address = String::from("file://");
    if generic.first() != Some(&b'/') {
        address.push('/'); // a drive letter
    }
    for &c in generic {
        if c.is_ascii_alphanumeric() || b"-_.~/:".contains(&c) {
            address.push(c as char);
        } else {
            address.push_str(&format!("%{c:02X}"));
        }
    }
    address
}
