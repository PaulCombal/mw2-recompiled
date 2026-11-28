// The converter's text: the names and strings of a zone, the names in a zip archive, a raw
// file's text, the paths it names in its log and messages (paths.rs). They are bytes,
// written back exactly as they were read, and kept as a String of one char per byte, the
// char of that value (Latin-1): the ASCII they nearly always are reads as itself, compares
// with literals and lowercases as ASCII, and any other byte goes back to itself. What
// prints or shows the converter's log and messages turns them back into their bytes
// (`bytes`), which are what a player's file and folder names were.

/// Bytes as text, a char for each.
pub fn of(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| char::from(b)).collect()
}

/// The bytes text of `of` stands for. (A char past 0xFF, which only text made otherwise
/// could hold, is written as its UTF-8.)
pub fn bytes(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    for c in text.chars() {
        if (c as u32) < 0x100 {
            out.push(c as u32 as u8);
        } else {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
    }
    out
}

/// Text of `of` as its bytes read as UTF-8 (a byte that isn't, as U+FFFD).
pub fn display(text: &str) -> String {
    if text.is_ascii() {
        return text.to_string();
    }
    String::from_utf8_lossy(&bytes(text)).into_owned()
}
