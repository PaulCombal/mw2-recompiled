// What the title says each player is doing, the line the console's guide and a
// friend's list showed: "Playing Team Deathmatch in Afghan".
//
// The title sets contexts (XUserSetContext): the presence mode under
// X_CONTEXT_PRESENCE, and the values its modes' strings name, such as the map. Its
// XLAST, the Live configuration gzipped into the SPA resource of its image (title.h's
// T_DATA_Spa), says what each mode and value reads as: a mode is a string with {c<id>}
// where a context's value goes. English, as XGetLanguage says.

use crate::env;
use crate::log::{log_info, log_warn};
use crate::runtime::{guest_memory, inflate};
use crate::title::*;
use std::collections::BTreeMap;
use std::ffi::CString;
use std::sync::Mutex;

const USERS: usize = 4;
const X_CONTEXT_PRESENCE: u32 = 0x00008001;
// std::string::npos, which the searches below add to and compare with as the C++ did.
const NONE: usize = usize::MAX;

// What the XLAST says, in English.
#[derive(Default)]
struct Context {
    fallback: u32,               // its defaultValue
    strings: BTreeMap<u32, u32>, // value -> string id
}

#[derive(Default)]
struct Strings {
    text: BTreeMap<u32, Vec<u8>>,     // string id -> English
    modes: BTreeMap<u32, u32>,        // presence mode -> string id
    contexts: BTreeMap<u32, Context>, // context id -> its values
}

struct State {
    contexts: [BTreeMap<u32, u32>; USERS],
    strings: Option<Strings>,
}

static STATE: Mutex<State> =
    Mutex::new(State { contexts: [BTreeMap::new(), BTreeMap::new(), BTreeMap::new(), BTreeMap::new()], strings: None });

fn find(text: &[u8], what: &[u8], from: usize) -> usize {
    if from > text.len() {
        return NONE;
    }
    text[from..].windows(what.len()).position(|part| part == what).map_or(NONE, |at| at + from)
}

// substr(), which takes a count that may be npos.
fn part(text: &[u8], from: usize, count: usize) -> &[u8] {
    &text[from..from.saturating_add(count).min(text.len())]
}

// An attribute of the tag starting at `at`, or "".
fn attribute<'a>(xml: &'a [u8], at: usize, name: &[u8]) -> &'a [u8] {
    let end = find(xml, b">", at);
    let key = [b" ", name, b"=\""].concat();
    let found = find(xml, &key, at);
    if found == NONE || found > end {
        return b"";
    }
    let from = found + key.len();
    part(xml, from, find(xml, b"\"", from).wrapping_sub(from))
}

fn number(text: &[u8]) -> u32 {
    let end = text.iter().position(|&byte| byte == 0).unwrap_or(text.len());
    CString::new(&text[..end]).map_or(0, |text| env::parse_any_base(&text) as u32)
}

fn replace_all(text: &mut Vec<u8>, from: &[u8], to: &[u8]) {
    loop {
        let at = find(text, from, 0);
        if at == NONE {
            return;
        }
        text.splice(at..at + from.len(), to.iter().copied());
    }
}

// The XML's five entities, the line's own spaces trimmed, and the registered mark the
// fonts do not have.
fn plain(mut text: Vec<u8>) -> Vec<u8> {
    const ENTITIES: [(&[u8], &[u8]); 6] =
        [(b"&lt;", b"<"), (b"&gt;", b">"), (b"&quot;", b"\""), (b"&apos;", b"'"), (b"\xC2\xAE", b""), (b"&amp;", b"&")];
    for (from, to) in ENTITIES {
        replace_all(&mut text, from, to);
    }
    while matches!(text.last(), Some(b' ' | b'\r' | b'\n' | b'\t' | 0)) {
        text.pop();
    }
    text
}

fn utf8(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() / 2);
    let unit = |i: usize| u32::from(data[i]) | u32::from(data[i + 1]) << 8;
    let mut i = 0;
    while i + 1 < data.len() {
        let mut c = unit(i);
        if (0xD800..0xDC00).contains(&c) && i + 3 < data.len() {
            c = 0x10000u32.wrapping_add((c - 0xD800) << 10).wrapping_add(unit(i + 2).wrapping_sub(0xDC00));
            i += 2;
        }
        if c < 0x80 {
            out.push(c as u8);
        } else if c < 0x800 {
            out.extend([0xC0 | (c >> 6) as u8, 0x80 | (c & 0x3F) as u8]);
        } else if c < 0x10000 {
            out.extend([0xE0 | (c >> 12) as u8, 0x80 | ((c >> 6) & 0x3F) as u8, 0x80 | (c & 0x3F) as u8]);
        } else {
            out.extend([
                (0xF0 | (c >> 18)) as u8,
                0x80 | ((c >> 12) & 0x3F) as u8,
                0x80 | ((c >> 6) & 0x3F) as u8,
                0x80 | (c & 0x3F) as u8,
            ]);
        }
        i += 2;
    }
    out
}

fn be32(data: &[u8], at: usize) -> usize {
    data.get(at..at + 4).map_or(0, |bytes| u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize)
}

// The XLAST's text: an XDBF whose metadata entry XSRC holds its name, then its size,
// then gzip.
fn xlast(spa: &[u8]) -> Vec<u8> {
    if !spa.starts_with(b"XDBF") {
        return Vec::new();
    }
    let (entries_max, entries, free_max) = (be32(spa, 8), be32(spa, 12), be32(spa, 16));
    let base = 24 + entries_max * 18 + free_max * 8;
    let mut i = 0;
    while i < entries && 24 + (i + 1) * 18 <= spa.len() {
        let entry = &spa[24 + i * 18..];
        i += 1;
        if entry[..2] != [0, 1] || &entry[6..10] != b"XSRC" || be32(entry, 2) != 0 {
            continue;
        }
        let (at, length) = (base + be32(entry, 10), be32(entry, 14));
        if at + length > spa.len() || length < 16 {
            return Vec::new();
        }
        let section = &spa[at..];
        let name = be32(section, 12);
        if 16 + name + 8 > length {
            return Vec::new();
        }
        let (size, packed) = (be32(section, 16 + name), be32(section, 20 + name));
        if 24 + name + packed > length {
            return Vec::new();
        }
        let Some(xml) = inflate::gunzip(&section[24 + name..24 + name + packed], size) else { return Vec::new() };
        // UTF-16, little-endian, after its byte order mark if it has one.
        return utf8(xml.strip_prefix(&[0xFF, 0xFE]).unwrap_or(&xml));
    }
    Vec::new()
}

fn parse(xml: &[u8]) -> Strings {
    let mut strings = Strings::default();
    let mut at = 0;
    loop {
        at = find(xml, b"<LocalizedString ", at);
        if at == NONE {
            break;
        }
        let end = find(xml, b"</LocalizedString>", at);
        let english = find(xml, b"<Translation locale=\"en-US\">", at);
        if end != NONE && english != NONE && english <= end {
            let from = find(xml, b">", english) + 1;
            let line = plain(part(xml, from, find(xml, b"<", from).wrapping_sub(from)).to_vec());
            strings.text.insert(number(attribute(xml, at, b"id")), line);
        }
        at += 1;
    }
    let mut at = 0;
    loop {
        at = find(xml, b"<PresenceMode ", at);
        if at == NONE {
            break;
        }
        strings.modes.insert(number(attribute(xml, at, b"contextValue")), number(attribute(xml, at, b"stringId")));
        at += 1;
    }
    let mut at = 0;
    loop {
        at = find(xml, b"<Context ", at);
        if at == NONE {
            break;
        }
        let context = strings.contexts.entry(number(attribute(xml, at, b"id"))).or_default();
        context.fallback = number(attribute(xml, at, b"defaultValue"));
        let end = find(xml, b"</Context>", at);
        let mut value = find(xml, b"<ContextValue ", at);
        while value < end {
            context.strings.insert(number(attribute(xml, value, b"value")), number(attribute(xml, value, b"stringId")));
            value = find(xml, b"<ContextValue ", value + 1);
        }
        at += 1;
    }
    strings
}

fn load() -> Strings {
    let memory = guest_memory::memory();
    let spa: Vec<u8> = (0..T_DATA_SpaSize).map(|i| memory.u8_at(T_DATA_Spa.wrapping_add(i))).collect();
    let xml = xlast(&spa);
    if xml.is_empty() {
        log_warn!("presence: the image has no XLAST to read");
        return Strings::default();
    }
    let strings = parse(&xml);
    log_info!(
        "presence: %llu modes, %llu contexts, %llu strings",
        strings.modes.len() as u64,
        strings.contexts.len() as u64,
        strings.text.len() as u64
    );
    strings
}

// The XLAST is read at the title's first context, on its thread, rather than when the
// guide first draws.
pub fn set_context(user: u32, id: u32, value: u32) {
    let user = user as usize;
    if user >= USERS {
        return;
    }
    let mut state = STATE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    state.strings.get_or_insert_with(load);
    state.contexts[user].insert(id, value);
}

/// The line for the player at this controller; empty before the title set a mode, or
/// where the image has no XLAST.
pub fn text(user: u32) -> Vec<u8> {
    let user = user as usize;
    if user >= USERS {
        return Vec::new();
    }
    let mut state = STATE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let State { contexts, strings } = &mut *state;
    let set = &contexts[user];
    let Some(mode) = set.get(&X_CONTEXT_PRESENCE) else { return Vec::new() };
    let strings = strings.get_or_insert_with(load);
    let Some(string) = strings.modes.get(mode) else { return Vec::new() };
    let text_of = |id: u32| strings.text.get(&id).cloned().unwrap_or_default();

    // "{c7}": the string of context 7's value. A property's ("{p...}") is left out: the
    // title's modes name none.
    let mut line = text_of(*string);
    let mut at = 0;
    loop {
        at = find(&line, b"{", at);
        if at == NONE {
            break;
        }
        let end = find(&line, b"}", at);
        if end == NONE {
            break;
        }
        let id = number(part(&line, at + 2, end.wrapping_sub(at + 2)));
        let mut value = Vec::new();
        if line[at + 1] == b'c'
            && let Some(found) = strings.contexts.get(&id)
        {
            let chosen = set.get(&id).copied().unwrap_or(found.fallback);
            if let Some(name) = found.strings.get(&chosen) {
                value = text_of(*name);
            }
        }
        line.splice(at..=end, value.iter().copied());
        at += value.len();
    }
    line
}
