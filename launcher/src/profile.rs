// The profile screen's work: what a player has earned, changed in the files the game
// keeps it in under saves/ (docs/saves.md).
//
// The multiplayer's rank, prestige and unlocks are in a stats file per player (mpdata);
// where each is in it comes from the title's own layout (playerdata_layout.rs). The
// campaign's progress is in the profile (profile.bin). A file is copied to
// <name>.backup before its first change.
#![deny(unsafe_code)]

use crate::boundary::utf8;
use crate::clock;
use crate::cnum::{c_str, strtoul};
use crate::playerdata_layout as layout;
use crate::setup::message;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const SAVES: &str = "saves";

fn shown(path: &Path) -> String {
    String::from_utf8_lossy(&utf8(path)).into_owned()
}

// The path with `suffix` after its name.
fn beside(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

// The file as it was before the launcher first touched it is kept beside it; the new
// contents go through a second file so that a write cut short leaves the old one.
fn write_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let (backup, partial) = (beside(path, ".backup"), beside(path, ".new"));
    if !backup.exists() {
        let _ = fs::copy(path, &backup);
    }
    fs::write(&partial, bytes).map_err(|_| format!("Cannot write {}.", shown(path)))?;
    fs::rename(&partial, path).map_err(|e| format!("Cannot write {}: {}", shown(path), message(&e)))
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

// ---- the multiplayer's stats ----------------------------------------------------------
// A stats file is a CRC-32 of the buffer, high byte first, then the buffer, 8188 bytes
// whose values are low byte first. The offline profile's, which the title saves as a
// content package, has four bytes before that and one after.
const STORED: usize = 8192;

struct Stats {
    file: Vec<u8>,
    lead: usize,
}

impl Stats {
    fn load(path: &Path) -> Option<Stats> {
        let file = fs::read(path).ok()?;
        let lead = match file.len() {
            STORED => 0,
            n if n == STORED + 5 => 4,
            _ => return None,
        };
        let stats = Stats { file, lead };
        // Another version of the game lays the buffer out its own way.
        (stats.get32(0) == layout::VERSION && stats.get32(4) == layout::FORMAT).then_some(stats)
    }

    fn buffer(&mut self) -> &mut [u8] {
        let at = self.lead + 4;
        &mut self.file[at..]
    }

    fn get32(&self, offset: u32) -> u32 {
        let at = self.lead + 4 + offset as usize;
        u32::from_le_bytes(self.file[at..at + 4].try_into().unwrap())
    }

    fn put32(&mut self, offset: u32, value: u32) {
        let at = offset as usize;
        self.buffer()[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn set_bit(&mut self, offset: u32, index: u16) {
        self.buffer()[offset as usize + usize::from(index / 8)] |= 1 << (index % 8);
    }

    fn save(&mut self, path: &Path) -> Result<(), String> {
        let lead = self.lead;
        let crc = crc32(&self.buffer()[..STORED - 4]);
        self.file[lead..lead + 4].copy_from_slice(&crc.to_be_bytes());
        write_file(path, &self.file)
    }
}

// The rank an amount of experience is: each starts at its own.
fn level(experience: u32) -> i32 {
    let mut level = 1;
    for (rank, &start) in layout::RANK_EXPERIENCE.iter().enumerate() {
        if experience >= start {
            level = rank as i32 + 1;
        }
    }
    level
}

// A change made to every stats file of a player; a file it leaves as it was is not
// written.
fn change(files: &[PathBuf], change: impl Fn(&mut Stats)) -> Result<(), String> {
    for file in files {
        let mut stats = Stats::load(file).ok_or_else(|| format!("Cannot read {}.", shown(file)))?;
        let before = stats.file.clone();
        change(&mut stats);
        if stats.file != before {
            stats.save(file)?;
        }
    }
    Ok(())
}

// The day the file was last written, "05 Mar 2026"; empty when it cannot be told.
fn day(path: &Path) -> String {
    let Ok(written) = fs::metadata(path).and_then(|metadata| metadata.modified()) else { return String::new() };
    clock::local(clock::seconds(written)).map(|local| clock::day(&local)).unwrap_or_default()
}

// ---- the campaign's progress ----------------------------------------------------------
// profile.bin is the settings the title wrote (src/runtime/kernel/profile.rs):
// "MW2PROF1", a count, then id, source, length and bytes of each, low byte first. The
// title's own settings are lists of numbered fields: "SEMV" and two words, then for each
// field its number, its type and its value -- a byte (types 1 and 2), 32 bits (4 and 5),
// or a string (6) as two 16-bit words, where it goes in the title's string area and how
// long it may be, and its characters up to a zero -- high byte first. The title has two
// such lists: one both of its executables read, which has the Special Ops star count the
// menus lock the mission groups by, and the campaign's own.
const SHARED_SETTING: u32 = 0x63E8_3FFF;
const CAMPAIGN_SETTING: u32 = 0x63E8_3FFE;
const STAR_COUNT: u8 = 0x0E; // shared
const HIGHEST_MISSION: u8 = 6; // campaign
const SPEC_OPS_DIFFICULTY: u8 = 0x23; // campaign
// The campaign's last level, and the Special Ops missions, whose stars are the difficulty
// each was finished at.
const MISSIONS: u32 = 20;
const SPEC_OPS: usize = 23;
const VETERAN: u8 = b'4';

const NO_PROFILE: &str = "There is no campaign profile yet: start the campaign once first.";

#[derive(Default)]
struct Field {
    number: u8,
    kind: u8,
    value: u32,
    place: u16,
    room: u16,
    text: Vec<u8>,
}

struct Fields {
    head: [u8; 12],
    fields: Vec<Field>,
}

impl Fields {
    fn parse(b: &[u8]) -> Option<Fields> {
        if b.len() < 12 || &b[..4] != b"SEMV" {
            return None;
        }
        let mut fields = Fields { head: b[..12].try_into().unwrap(), fields: Vec::new() };
        let mut at = 12;
        while at < b.len() {
            if at + 2 > b.len() {
                return None;
            }
            let mut field = Field { number: b[at], kind: b[at + 1], ..Field::default() };
            at += 2;
            let size = match field.kind {
                1 | 2 => 1,
                4..=6 => 4,
                _ => 0,
            };
            if size == 0 || at + size > b.len() {
                return None;
            }
            if field.kind == 6 {
                field.place = u16::from_be_bytes([b[at], b[at + 1]]);
                field.room = u16::from_be_bytes([b[at + 2], b[at + 3]]);
                at += 4;
                let end = at + b[at..].iter().position(|&byte| byte == 0)?;
                field.text = b[at..end].to_vec();
                at = end + 1;
            } else {
                field.value = b[at..at + size].iter().fold(0, |value, &byte| value << 8 | u32::from(byte));
                at += size;
            }
            fields.fields.push(field);
        }
        Some(fields)
    }

    fn find(&mut self, number: u8, kind: u8) -> Option<&mut Field> {
        self.fields.iter_mut().find(|field| field.number == number && field.kind == kind)
    }

    fn bytes(&self) -> Vec<u8> {
        let mut b = self.head.to_vec();
        for field in &self.fields {
            b.push(field.number);
            b.push(field.kind);
            match field.kind {
                6 => {
                    b.extend_from_slice(&field.place.to_be_bytes());
                    b.extend_from_slice(&field.room.to_be_bytes());
                    b.extend_from_slice(&field.text);
                    b.push(0);
                }
                1 | 2 => b.push(field.value as u8),
                _ => b.extend_from_slice(&field.value.to_be_bytes()),
            }
        }
        b
    }
}

struct Setting {
    id: u32,
    source: u32,
    bytes: Vec<u8>,
}

struct Settings {
    settings: Vec<Setting>,
    shared: Fields,
    campaign: Fields,
}

impl Settings {
    fn path() -> PathBuf {
        Path::new(SAVES).join("profile.bin")
    }

    fn load() -> Option<Settings> {
        let file = fs::read(Self::path()).ok()?;
        if file.len() < 12 || &file[..8] != b"MW2PROF1" {
            return None;
        }
        let word = |at: usize| u32::from_le_bytes(file[at..at + 4].try_into().unwrap());
        let mut settings = Vec::new();
        let mut at = 12;
        for _ in 0..word(8) {
            if at + 12 > file.len() {
                return None;
            }
            let (id, source, length) = (word(at), word(at + 4), word(at + 8) as usize);
            at += 12;
            if at + length > file.len() {
                return None;
            }
            settings.push(Setting { id, source, bytes: file[at..at + length].to_vec() });
            at += length;
        }
        let find = |id: u32| settings.iter().find(|setting| setting.id == id);
        let shared = Fields::parse(&find(SHARED_SETTING)?.bytes)?;
        let campaign = Fields::parse(&find(CAMPAIGN_SETTING)?.bytes)?;
        Some(Settings { settings, shared, campaign })
    }

    fn save(&mut self) -> Result<(), String> {
        let (shared, campaign) = (self.shared.bytes(), self.campaign.bytes());
        for setting in &mut self.settings {
            if setting.id == SHARED_SETTING {
                setting.bytes = shared.clone();
            } else if setting.id == CAMPAIGN_SETTING {
                setting.bytes = campaign.clone();
            }
        }
        let mut file = b"MW2PROF1".to_vec();
        file.extend_from_slice(&(self.settings.len() as u32).to_le_bytes());
        for setting in &self.settings {
            file.extend_from_slice(&setting.id.to_le_bytes());
            file.extend_from_slice(&setting.source.to_le_bytes());
            file.extend_from_slice(&(setting.bytes.len() as u32).to_le_bytes());
            file.extend_from_slice(&setting.bytes);
        }
        write_file(&Self::path(), &file)
    }
}

// ---- the profiles ---------------------------------------------------------------------
// saves/profiles.txt, the game's (src/runtime/signin.rs): a line per profile, twelve
// hexadecimal digits, a space, the name; `first`, the profile at the first controller;
// and lines of the game's own, kept as they are.

struct Named {
    id: u64,
    name: Vec<u8>,
}

#[derive(Default)]
struct Book {
    named: Vec<Named>,
    first: u64,
    others: Vec<Vec<u8>>,
}

fn book_path() -> PathBuf {
    Path::new(SAVES).join("profiles.txt")
}

fn read_book() -> Book {
    let mut book = Book::default();
    let text = fs::read(book_path()).unwrap_or_default();
    let mut lines: Vec<&[u8]> = text.split(|&byte| byte == b'\n').collect();
    if text.is_empty() || text.last() == Some(&b'\n') {
        lines.pop();
    }
    for mut line in lines {
        while let [rest @ .., b'\r' | b' '] = line {
            line = rest;
        }
        if let Some(rest) = line.strip_prefix(b"first ") {
            book.first = strtoul(c_str(rest), 16, 64).0;
            continue;
        }
        // Twelve digits, all of them read (up to a zero byte, where the C's string ended).
        let id = if line.len() >= 14 && line[12] == b' ' {
            let digits = c_str(&line[..12]);
            match strtoul(digits, 16, 64) {
                (id, Some(end)) if end == digits.len() => id,
                _ => 0,
            }
        } else {
            0
        };
        if id != 0 {
            book.named.push(Named { id, name: line[13..line.len().min(28)].to_vec() });
        } else if !line.is_empty() {
            book.others.push(line.to_vec());
        }
    }
    book
}

fn id_text(id: u64) -> String {
    format!("{id:012x}")
}

fn write_book(book: &Book) -> Result<(), String> {
    let mut text = Vec::new();
    for line in &book.others {
        text.extend_from_slice(line);
        text.push(b'\n');
    }
    if book.first != 0 {
        text.extend_from_slice(format!("first {}\n", id_text(book.first)).as_bytes());
    }
    for entry in &book.named {
        text.extend_from_slice(id_text(entry.id).as_bytes());
        text.push(b' ');
        text.extend_from_slice(&entry.name);
        text.push(b'\n');
    }
    fs::write(book_path(), text).map_err(|_| "saves/profiles.txt could not be written.".to_string())
}

/// A new name for a profile: up to fifteen letters, digits and spaces.
pub fn rename(id: u64, name: &[u8]) -> Result<(), String> {
    if name.is_empty()
        || name.len() > 15
        || name[0] == b' '
        || name[name.len() - 1] == b' '
        || !name.iter().all(|&c| c.is_ascii_alphanumeric() || c == b' ')
    {
        return Err("A name is one to fifteen letters, digits and spaces.".into());
    }
    let mut book = read_book();
    let mut found = false;
    for entry in &mut book.named {
        if entry.id == id {
            entry.name = name.to_vec();
            found = true;
        } else if entry.name == name {
            return Err("Another profile has that name.".into());
        }
    }
    if !found {
        return Err("No profile owns these stats any more.".into());
    }
    write_book(&book)
}

/// Puts a profile at the first controller, from the game's next start.
pub fn play_as(id: u64) -> Result<(), String> {
    let mut book = read_book();
    if !book.named.iter().any(|entry| entry.id == id) {
        return Err("No profile owns these stats any more.".into());
    }
    book.first = id;
    write_book(&book)
}

/// A profile's other stats file, with what it holds: it has one for split screen and
/// system link with no service, and one the online service keeps.
#[derive(Clone)]
pub struct Other {
    pub file: PathBuf,
    pub offline: bool,
    pub level: i32,
    pub prestige: i32,
    pub played: String,
}

/// One player's multiplayer stats.
#[derive(Clone, Default)]
pub struct Player {
    // A profile (saves/profiles.txt, src/runtime/signin.rs) has a name and a number there,
    // and no stats file until its first match ends. A stats file with neither is one no
    // profile owns any more.
    pub name: Vec<u8>,
    pub id: u64,
    pub file: PathBuf,
    // `file` is the one played last; a change is made to both.
    pub also: Vec<Other>,
    pub offline: bool, // the profile nobody signed in to an online service with
    pub first: bool,   // the profile at the first controller
    pub level: i32,
    pub prestige: i32,
    pub played: String, // the day the file was last written
}

/// Every stats file under saves/ this version of the game can read, the one played last
/// first.
pub fn players() -> Vec<Player> {
    let mut found: Vec<(Player, Option<SystemTime>)> = Vec::new();
    let mut add = |file: PathBuf, offline: bool| {
        let Some(stats) = Stats::load(&file) else { return };
        let written = fs::metadata(&file).and_then(|metadata| metadata.modified()).ok();
        let player = Player {
            level: level(stats.get32(layout::EXPERIENCE)),
            prestige: stats.get32(layout::PRESTIGE) as i32,
            played: day(&file),
            file,
            offline,
            ..Player::default()
        };
        found.push((player, written));
    };
    let saves = Path::new(SAVES);
    // What the online service keeps for each player who signed in...
    if let Ok(entries) = fs::read_dir(saves.join("online").join("user")) {
        for entry in entries.flatten() {
            add(entry.path().join("mpdata"), false);
        }
    }
    // ...and the content package the title saves the offline profile's in.
    if let Ok(entries) = fs::read_dir(saves) {
        for entry in entries.flatten() {
            if entry.file_name().as_encoded_bytes().starts_with(b"mpdata_") {
                add(entry.path().join(entry.file_name()), true);
            }
        }
    }
    // Newest first; a file whose time cannot be read is the oldest.
    found.sort_by_key(|(_, written)| std::cmp::Reverse(*written));
    let mut players: Vec<Player> = found.into_iter().map(|(player, _)| player).collect();
    // The profiles: the stats files of one are named by its number -- the offline package
    // and what the online service keeps -- and one that has not finished a match has none.
    let book = read_book();
    for entry in &book.named {
        let id = id_text(entry.id);
        let (package, online) = (format!("mpdata_e000{id}"), format!("0009{id}"));
        let mut owned = false;
        for player in &mut players {
            let name = player.file.file_name().map(|name| name.as_encoded_bytes().to_vec()).unwrap_or_default();
            let parent = player
                .file
                .parent()
                .and_then(Path::file_name)
                .map(|name| name.as_encoded_bytes().to_vec())
                .unwrap_or_default();
            if name != package.as_bytes() && parent != online.as_bytes() {
                continue;
            }
            player.name = entry.name.clone();
            player.id = entry.id;
            player.first = entry.id == book.first;
            owned = true;
        }
        if !owned {
            players.push(Player {
                offline: true,
                name: entry.name.clone(),
                id: entry.id,
                first: entry.id == book.first,
                level: 1,
                ..Player::default()
            });
        }
    }
    // One entry a profile: the list is the newest file first, so the first of a profile's
    // is the one played last.
    let mut merged: Vec<Player> = Vec::new();
    for player in players {
        match merged.iter_mut().find(|other| player.id != 0 && other.id == player.id) {
            None => merged.push(player),
            Some(same) => same.also.push(Other {
                file: player.file,
                offline: player.offline,
                level: player.level,
                prestige: player.prestige,
                played: player.played,
            }),
        }
    }
    merged
}

pub const MAX_LEVEL: i32 = layout::RANK_EXPERIENCE.len() as i32;
pub const MAX_PRESTIGE: i32 = layout::MAX_PRESTIGE as i32;

/// The rank, 1 to MAX_LEVEL, and the prestige, 0 to MAX_PRESTIGE, put in each of the
/// player's stats files; a level of 0 or a prestige of -1 leaves that one as each file has
/// it. A file at the rank keeps its experience, and a file that holds both already is not
/// written. Rank 70 is the end of its experience bar.
pub fn set_rank(files: &[PathBuf], level_asked: i32, prestige: i32) -> Result<(), String> {
    change(files, |stats| {
        // A file at the rank already keeps its experience, however far into it.
        if level_asked > 0 && level(stats.get32(layout::EXPERIENCE)) != level_asked {
            let rank = level_asked.clamp(1, MAX_LEVEL);
            let experience =
                if rank == MAX_LEVEL { layout::MAX_EXPERIENCE } else { layout::RANK_EXPERIENCE[rank as usize - 1] };
            stats.put32(layout::EXPERIENCE, experience);
        }
        if prestige >= 0 {
            stats.put32(layout::PRESTIGE, prestige.clamp(0, MAX_PRESTIGE) as u32);
        }
    })
}

/// Every challenge done, and every title, emblem and killstreak unlocked.
pub fn unlock_everything(files: &[PathBuf]) -> Result<(), String> {
    change(files, |stats| {
        for challenge in &layout::CHALLENGES {
            stats.buffer()[(layout::CHALLENGE_STATE + u32::from(challenge.index)) as usize] = challenge.tiers + 1;
            stats.put32(layout::CHALLENGE_PROGRESS + 4 * u32::from(challenge.index), challenge.target);
        }
        for &index in &layout::TITLES {
            stats.set_bit(layout::TITLE_UNLOCKED, index);
        }
        for &index in &layout::ICONS {
            stats.set_bit(layout::ICON_UNLOCKED, index);
        }
        for &index in &layout::KILLSTREAKS {
            stats.set_bit(layout::KILLSTREAK_UNLOCKED, index);
        }
    })
}

/// The game's volume as the player set it in its options: the float the shared settings
/// keep as field 6, which the title gives its snd_volume. None without a profile yet.
pub fn volume() -> Option<f32> {
    const VOLUME: u8 = 6;
    let mut settings = Settings::load()?;
    Some(f32::from_bits(settings.shared.find(VOLUME, 5)?.value))
}

/// The campaign's progress, as the profile has it: None when there is no profile yet,
/// else how far the campaign is unlocked and the Special Ops stars.
pub fn read_campaign() -> Option<(i32, i32)> {
    let mut settings = Settings::load()?;
    let missions = settings.campaign.find(HIGHEST_MISSION, 1)?.value as i32;
    let stars = settings.shared.find(STAR_COUNT, 4)?.value as i32;
    Some((missions, stars))
}

pub fn unlock_missions() -> Result<(), String> {
    let mut settings = Settings::load().ok_or(NO_PROFILE)?;
    let highest = settings.campaign.find(HIGHEST_MISSION, 1).ok_or(NO_PROFILE)?;
    highest.value = highest.value.max(MISSIONS);
    settings.save()
}

pub fn all_stars() -> Result<(), String> {
    let mut settings = Settings::load().ok_or(NO_PROFILE)?;
    if settings.shared.find(STAR_COUNT, 4).is_none() {
        return Err(NO_PROFILE.into());
    }
    let stars = settings.campaign.find(SPEC_OPS_DIFFICULTY, 6).ok_or(NO_PROFILE)?;
    if usize::from(stars.room) < SPEC_OPS {
        return Err(NO_PROFILE.into());
    }
    // A mission finished on veteran has its three stars, and the count the menus open the
    // mission groups by is their sum.
    if stars.text.len() < SPEC_OPS {
        stars.text.resize(SPEC_OPS, b'0');
    }
    stars.text[..SPEC_OPS].fill(VETERAN);
    settings.shared.find(STAR_COUNT, 4).ok_or(NO_PROFILE)?.value = 3 * SPEC_OPS as u32;
    settings.save()
}
