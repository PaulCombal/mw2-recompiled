// Custom maps in the multiplayer's menus.
//
// Two lists name the maps. The arena table: UI_LoadArenas parses mp/basemaps.arena,
// a raw file in patch_mp.ff, into up to 64 info strings, and UI_UpdateArenas takes
// each one's map, longname, description and mapimage into the table the lobby looks
// a map's name and picture up in. And the menus the host chooses a map from,
// settings_map (the game's 16) and settings_bonus_map (the downloadable ones), also
// in patch_mp.ff: a button each, written out by hand, whose action sets ui_mapname.
//
// A map converted from the PC game comes with its own .arena, in the same format,
// which the PC game reads from its folder (the 360 title keeps only that path's
// message, ".arena files : %s"). With MW2_CUSTOM_MAPS, for every <map>.arena in the
// game folder whose <map>.ff is there too:
//
//   - the arena is parsed after the title's own, with the title's parser, so the
//     lobby knows the map;
//   - the bonus menu gets a button for each map in it, below its own: a copy of its
//     last map button with the map's name in its action, its label and the three
//     local variables its focus sets for the side panel (ui_info_name, ui_info_desc,
//     ui_info_icon). The copy leaves out the downloadable maps' colour expression,
//     which greys a map the console has not bought.
//
// The longname and description are shown as the arena has them: the menus' "@"
// lookup answers them with themselves.
//
// The side panel's picture is a material, preview_<map>, which a stock map has in
// ui_mp.ff and a downloadable one in dlc1_ui_mp.ff or dlc2_ui_mp.ff, zones
// Com_LoadUiFastFile loads in one DB_LoadXAssets call. A converted map comes with
// its own, made of its loading screen: <map>_preview.ff, which that call loads too,
// after the title's, with the same flags (a level's load frees them). A map without
// one shows black.
//
// UI_UpdateArenas runs UI_LoadArenas when the menus first need the table (the
// private match's lobby), not at start, and again when it is marked stale; the menu
// is patched the first time, and again if it is ever loaded anew.
//
// What the hooks keep between guest calls is in statics, so that no Vec is held
// across one (hooks/mod.rs). The guest memory they make comes from the kernel's
// heap, in the C++'s order, so that the two give the same addresses.

use crate::env;
use crate::guest::{Memory, PPCContext, Register, guest_hook, preserving};
use crate::log::{log_info, log_warn};
use crate::runtime::install::arena::{self, Map};
use crate::runtime::kernel;
use crate::title::*;
use std::cell::Cell;
use std::collections::BTreeMap;
use std::ffi::{CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{LazyLock, Mutex, MutexGuard, OnceLock, PoisonError};

const ROOM: u32 = 64; // the info strings the table holds
const BUFFER_SIZE: u32 = 0x18000; // UI_LoadArenas' own, ARENA_BUFFER_SIZE
const MENU_ASSET: u32 = 24;

// menuDef_t and itemDef_s, as the 360 lays them out (launcher/zone/defs/iw4_assets.h).
const MENU_ITEM_COUNT: u32 = 184;
const MENU_ITEMS: u32 = 296;
const ITEM_SIZE: u32 = 460;
// rect is the item's position as written; rectClient (+24) the one the menu places
// and draws it by, made from it when the menu loaded.
const ITEM_RECT_Y: u32 = 8;
const ITEM_RECT_CLIENT_Y: u32 = 28;
const ITEM_ACTION: u32 = 328;
const ITEM_ON_FOCUS: u32 = 336;
const ITEM_ENABLE_DVAR: u32 = 356;
const ITEM_FLOAT_EXPRESSION_COUNT: u32 = 396;
const ITEM_FLOAT_EXPRESSIONS: u32 = 400;
const ITEM_TEXT_EXP: u32 = 412;
// A button 20 units below the one before; the menu's frame ends at 384.
const ROW_HEIGHT: f32 = 20.0;
const LIST_END: f32 = 368.0;

const EVENT_UNCONDITIONAL: u8 = 0;
const EVENT_SET_LOCAL_VAR_STRING: u8 = 6;
const OPERAND: u32 = 1;
const VAL_STRING: u32 = 2;

// XZoneInfo { name, allocFlags, freeFlags }, as DB_LoadXAssets takes a list of them.
const ZONE_INFO_SIZE: u32 = 12;
const MOST_ZONES: u32 = 32; // what the loader holds: one more stops the game
const ZONE_NAME_SIZE: u32 = 64;

const VARIABLES: [&CStr; 3] = [c"ui_info_name", c"ui_info_desc", c"ui_info_icon"];

static CUSTOM_MAPS: LazyLock<bool> = LazyLock::new(|| env::flag(c"MW2_CUSTOM_MAPS"));

// One UI_LoadArenas: the arenas installed, and the maps of those parsed so far.
struct Pass {
    files: Vec<PathBuf>,
    maps: Vec<Map>,
}
static PASS: Mutex<Pass> = Mutex::new(Pass { files: Vec::new(), maps: Vec::new() });

// The maps whose picture the menus' zones have, by map, and the picture:
// <name>_preview.ff loaded with them, named after its .arena as the launcher installs
// it, holds preview_<name>. Made anew each time the menus load.
static PREVIEWS: Mutex<BTreeMap<Vec<u8>, Vec<u8>>> = Mutex::new(BTreeMap::new());

// The item array this hook last gave the bonus menu.
static PATCHED_ITEMS: AtomicU32 = AtomicU32::new(0);

// The longnames and descriptions the custom maps show, which the menus look up as
// translations. Set once, before the first lookup that could want them.
static TEXTS: OnceLock<Vec<Vec<u8>>> = OnceLock::new();

thread_local! {
    static LOADING_MENUS: Cell<bool> = const { Cell::new(false) };
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn plural(n: u32) -> &'static CStr {
    if n == 1 { c"" } else { c"s" }
}

// A text for the log, as printf shows a std::string's c_str(): up to its first zero.
fn shown(bytes: &[u8]) -> CString {
    let end = bytes.iter().position(|&byte| byte == 0).unwrap_or(bytes.len());
    CString::new(&bytes[..end]).unwrap_or_default()
}

fn file_name(path: &Path) -> Vec<u8> {
    path.file_name().map(|name| name.as_encoded_bytes().to_vec()).unwrap_or_default()
}

fn file_stem(path: &Path) -> Vec<u8> {
    path.file_stem().map(|stem| stem.as_encoded_bytes().to_vec()).unwrap_or_default()
}

// Zeroed guest memory, or 0 when there is none (or none was asked for).
fn alloc(memory: Memory, size: u32) -> u32 {
    let at = kernel::allocate_guest(size, 16);
    if at != 0 {
        memory.zero(at, size);
    }
    at
}

fn guest_string(memory: Memory, text: &[u8]) -> u32 {
    let size = (text.len() as u32).wrapping_add(1);
    let at = alloc(memory, size);
    if at != 0 {
        memory.store_bytes(at, text);
        memory.store_u8(at.wrapping_add(text.len() as u32), 0);
    }
    at
}

// strcmp(string, text) == 0 for a string that is not null; `text` holds no zero.
fn equals(memory: Memory, string: u32, text: &[u8]) -> bool {
    if string == 0 {
        return false;
    }
    for (i, &wanted) in text.iter().enumerate() {
        if memory.u8_at(string.wrapping_add(i as u32)) != wanted {
            return false;
        }
    }
    memory.u8_at(string.wrapping_add(text.len() as u32)) == 0
}

fn guest_text(memory: Memory, at: u32) -> Vec<u8> {
    memory.string(at, usize::MAX)
}

// A copy of a Statement_s whose string operands equal to `from` are `to`.
fn copy_statement(memory: Memory, statement: u32, from: &[u8], to: u32) -> u32 {
    if statement == 0 {
        return 0;
    }
    let copy = alloc(memory, 24);
    memory.copy(copy, statement, 24);
    let count = memory.u32_at(statement);
    let entries = memory.u32_at(statement.wrapping_add(4));
    let made = alloc(memory, count.wrapping_mul(12));
    memory.copy(made, entries, count.wrapping_mul(12));
    for i in 0..count {
        let entry = made.wrapping_add(i.wrapping_mul(12));
        if memory.u32_at(entry) == OPERAND
            && memory.u32_at(entry.wrapping_add(4)) == VAL_STRING
            && equals(memory, memory.u32_at(entry.wrapping_add(8)), from)
        {
            memory.store_u32(entry.wrapping_add(8), to);
        }
    }
    memory.store_u32(copy.wrapping_add(4), made);
    copy
}

// The string a statement holds, the nth: the map's own in a button's.
fn statement_string(memory: Memory, statement: u32, mut nth: usize) -> Vec<u8> {
    if statement == 0 {
        return Vec::new();
    }
    let count = memory.u32_at(statement);
    let entries = memory.u32_at(statement.wrapping_add(4));
    for i in 0..count {
        let entry = entries.wrapping_add(i.wrapping_mul(12));
        if memory.u32_at(entry) == OPERAND && memory.u32_at(entry.wrapping_add(4)) == VAL_STRING {
            if nth == 0 {
                return guest_text(memory, memory.u32_at(entry.wrapping_add(8)));
            }
            nth -= 1;
        }
    }
    Vec::new()
}

// MenuEventHandlerSet { count, MenuEventHandler** }; MenuEventHandler { data, type }.
fn new_handler_set(memory: Memory, handlers: &[u32]) -> u32 {
    let set = alloc(memory, 8);
    let list = alloc(memory, (handlers.len() as u32).wrapping_mul(4));
    memory.store_u32(set, handlers.len() as u32);
    memory.store_u32(set.wrapping_add(4), list);
    for (i, &handler) in handlers.iter().enumerate() {
        memory.store_u32(list.wrapping_add(i as u32 * 4), handler);
    }
    set
}

fn new_script(memory: Memory, script: &[u8]) -> u32 {
    let handler = alloc(memory, 8);
    memory.store_u32(handler, guest_string(memory, script));
    memory.store_u8(handler.wrapping_add(4), EVENT_UNCONDITIONAL);
    handler
}

// The template's focus handlers, with the side panel's three variables set to the map's.
fn copy_focus(memory: Memory, set: u32, map: &Map, was: &[Vec<u8>; 3]) -> u32 {
    let picture = lock(&PREVIEWS).get(&map.map).cloned().unwrap_or_else(|| b"black".to_vec());
    let is = [&map.longname, &map.description, &picture];
    let mut handlers = Vec::new();
    let count = memory.u32_at(set);
    let list = memory.u32_at(set.wrapping_add(4));
    for i in 0..count {
        let mut handler = memory.u32_at(list.wrapping_add(i.wrapping_mul(4)));
        let data = memory.u32_at(handler);
        if memory.u8_at(handler.wrapping_add(4)) == EVENT_SET_LOCAL_VAR_STRING {
            for v in 0..3 {
                if equals(memory, memory.u32_at(data), VARIABLES[v].to_bytes()) {
                    // SetLocalVarData { name, expression }
                    let set_var = alloc(memory, 8);
                    memory.store_u32(set_var, memory.u32_at(data));
                    let to = guest_string(memory, is[v]);
                    let expression = copy_statement(memory, memory.u32_at(data.wrapping_add(4)), &was[v], to);
                    memory.store_u32(set_var.wrapping_add(4), expression);
                    handler = alloc(memory, 8);
                    memory.store_u32(handler, set_var);
                    memory.store_u8(handler.wrapping_add(4), EVENT_SET_LOCAL_VAR_STRING);
                }
            }
        }
        handlers.push(handler);
    }
    new_handler_set(memory, &handlers)
}

fn load_float(memory: Memory, at: u32) -> f32 {
    f32::from_bits(memory.u32_at(at))
}

fn store_float(memory: Memory, at: u32, value: f32) {
    memory.store_u32(at, value.to_bits());
}

// The bonus menu's own buttons and then one for each custom map of this pass.
fn add_buttons(ctx: &mut PPCContext, memory: Memory) {
    let menu = preserving(ctx, |ctx| {
        let name = ctx.r1.as_u32().wrapping_sub(512);
        memory.store_bytes(name, b"settings_bonus_map\0");
        ctx.r1.u64 = u64::from(ctx.r1.as_u32().wrapping_sub(1024));
        ctx.r3.u64 = u64::from(MENU_ASSET);
        ctx.r4.u64 = u64::from(name);
        T_DB_FindXAssetHeader.call(ctx, memory);
        ctx.r3.as_u32()
    });
    if menu == 0 {
        log_warn!("maps: no settings_bonus_map menu");
        return;
    }
    let items = memory.u32_at(menu.wrapping_add(MENU_ITEMS));
    let count = memory.u32_at(menu.wrapping_add(MENU_ITEM_COUNT));
    if items == PATCHED_ITEMS.load(Ordering::Relaxed) {
        return;
    }

    // The template: the last button whose action sets ui_mapname.
    let (mut last, mut last_index) = (0, 0);
    for i in 0..count {
        let item = memory.u32_at(items.wrapping_add(i.wrapping_mul(4)));
        if memory.u32_at(item.wrapping_add(ITEM_ENABLE_DVAR)) != 0
            && memory.u32_at(item.wrapping_add(ITEM_ACTION)) != 0
            && memory.u32_at(item.wrapping_add(ITEM_ON_FOCUS)) != 0
            && !statement_string(memory, memory.u32_at(item.wrapping_add(ITEM_TEXT_EXP)), 1).is_empty()
        {
            last = item;
            last_index = i;
        }
    }
    if last == 0 {
        log_warn!("maps: settings_bonus_map has no map button to copy");
        return;
    }
    let label = statement_string(memory, memory.u32_at(last.wrapping_add(ITEM_TEXT_EXP)), 1);
    // What the template's focus sets, to find the strings to replace.
    let mut was: [Vec<u8>; 3] = Default::default();
    {
        let set = memory.u32_at(last.wrapping_add(ITEM_ON_FOCUS));
        let n = memory.u32_at(set);
        let list = memory.u32_at(set.wrapping_add(4));
        for i in 0..n {
            let handler = memory.u32_at(list.wrapping_add(i.wrapping_mul(4)));
            let data = memory.u32_at(handler);
            if memory.u8_at(handler.wrapping_add(4)) != EVENT_SET_LOCAL_VAR_STRING {
                continue;
            }
            for v in 0..3 {
                if equals(memory, memory.u32_at(data), VARIABLES[v].to_bytes()) {
                    was[v] = statement_string(memory, memory.u32_at(data.wrapping_add(4)), 0);
                }
            }
        }
    }
    // "play" "mouse_click"
    let click = memory.u32_at(memory.u32_at(memory.u32_at(last.wrapping_add(ITEM_ACTION)).wrapping_add(4)));

    let maps = std::mem::take(&mut lock(&PASS).maps);
    let mut added = Vec::new();
    let mut y = load_float(memory, last.wrapping_add(ITEM_RECT_Y)) + ROW_HEIGHT;
    for map in &maps {
        if y > LIST_END {
            log_warn!("maps: no room in the menu for %s", shown(&map.map).as_c_str());
            continue;
        }
        let item = alloc(memory, ITEM_SIZE);
        memory.copy(item, last, ITEM_SIZE);
        store_float(memory, item.wrapping_add(ITEM_RECT_Y), y);
        let client = load_float(memory, last.wrapping_add(ITEM_RECT_CLIENT_Y))
            + (y - load_float(memory, last.wrapping_add(ITEM_RECT_Y)));
        store_float(memory, item.wrapping_add(ITEM_RECT_CLIENT_Y), client);
        y += ROW_HEIGHT;
        let enable = [b"\"".as_slice(), &map.map, b"\" "].concat();
        memory.store_u32(item.wrapping_add(ITEM_ENABLE_DVAR), guest_string(memory, &enable));
        memory.store_u32(item.wrapping_add(ITEM_FLOAT_EXPRESSION_COUNT), 0);
        memory.store_u32(item.wrapping_add(ITEM_FLOAT_EXPRESSIONS), 0);
        let longname = guest_string(memory, &map.longname);
        let text = copy_statement(memory, memory.u32_at(last.wrapping_add(ITEM_TEXT_EXP)), &label, longname);
        memory.store_u32(item.wrapping_add(ITEM_TEXT_EXP), text);
        let script = [
            b"\"setdvar\" \"ui_mapname\" \"".as_slice(),
            &map.map,
            b"\" ; \"close\" \"settings_map\" ; \"close\" \"self\" ; ",
        ]
        .concat();
        let script = new_script(memory, &script);
        memory.store_u32(item.wrapping_add(ITEM_ACTION), new_handler_set(memory, &[click, script]));
        let focus = copy_focus(memory, memory.u32_at(last.wrapping_add(ITEM_ON_FOCUS)), map, &was);
        memory.store_u32(item.wrapping_add(ITEM_ON_FOCUS), focus);
        added.push(item);
    }
    if added.is_empty() {
        return;
    }

    // The new buttons go after the template, where the menu's own order of focus
    // has them follow it.
    let more = added.len() as u32;
    let made = alloc(memory, count.wrapping_add(more).wrapping_mul(4));
    let mut at = 0u32;
    for i in 0..count {
        memory.store_u32(made.wrapping_add(at.wrapping_mul(4)), memory.u32_at(items.wrapping_add(i.wrapping_mul(4))));
        at = at.wrapping_add(1);
        if i == last_index {
            for &item in &added {
                memory.store_u32(made.wrapping_add(at.wrapping_mul(4)), item);
                at = at.wrapping_add(1);
            }
        }
    }
    memory.store_u32(menu.wrapping_add(MENU_ITEMS), made);
    memory.store_u32(menu.wrapping_add(MENU_ITEM_COUNT), count.wrapping_add(more));
    PATCHED_ITEMS.store(made, Ordering::Relaxed);
    log_info!("maps: %u custom map%s in the bonus maps menu", more, plural(more));
}

guest_hook!(T_UI_LoadArenas, |ctx, memory| {
    T_UI_LoadArenas.original(ctx, memory);
    if !*CUSTOM_MAPS {
        return;
    }
    {
        let mut pass = lock(&PASS);
        pass.files = arena::installed(&kernel::game_root());
        pass.maps.clear();
    }

    preserving(ctx, |ctx| {
        // A frame of its own below the caller's, for the buffer's handle.
        ctx.r1.u64 = u64::from(ctx.r1.as_u32().wrapping_sub(128));
        let handle = ctx.r1.as_u32().wrapping_add(80);
        ctx.r3.u64 = u64::from(handle);
        ctx.r4.u64 = u64::from(BUFFER_SIZE);
        T_LargeLocal_Begin.call(ctx, memory);
        ctx.r3.u64 = u64::from(handle);
        T_LargeLocal_GetBuf.call(ctx, memory);
        let buffer = ctx.r3.as_u32();

        for index in 0.. {
            let count = memory.u32_at(T_DATA_UI_ArenaInfos + 256);
            {
                let mut pass = lock(&PASS);
                let Some(file) = pass.files.get(index) else { break };
                let name = shown(&file_name(file));
                if count >= ROOM {
                    log_warn!("maps: %s left out, the map list is full", name.as_c_str());
                    break;
                }
                let mut text = arena::read(file);
                if text.len() >= BUFFER_SIZE as usize {
                    log_warn!(
                        "maps: %s left out, it is larger than the title's %u bytes",
                        name.as_c_str(),
                        BUFFER_SIZE
                    );
                    continue;
                }
                let maps = arena::maps(&text);
                pass.maps.extend(maps);
                text.push(0);
                memory.store_bytes(buffer, &text);
            }
            ctx.r3.u64 = u64::from(buffer);
            ctx.r4.u64 = u64::from(ROOM - count);
            ctx.r5.u64 = u64::from(T_DATA_UI_ArenaInfos + count * 4);
            T_UI_ParseInfos.call(ctx, memory);
            let added = ctx.r3.as_u32();
            memory.store_u32(T_DATA_UI_ArenaInfos + 256, count.wrapping_add(added));
            let name = shown(&lock(&PASS).files.get(index).map(|file| file_name(file)).unwrap_or_default());
            log_info!("maps: %s adds %u map%s to the list", name.as_c_str(), added, plural(added));
        }

        ctx.r3.u64 = u64::from(handle);
        T_LargeLocal_End.call(ctx, memory);
    });

    let found = !lock(&PASS).maps.is_empty();
    if found && TEXTS.get().is_none() {
        let texts = lock(&PASS)
            .maps
            .iter()
            .flat_map(|map| [map.longname.clone(), map.description.clone()])
            .filter(|text| !text.is_empty())
            .collect();
        let _ = TEXTS.set(texts);
    }
    if found {
        add_buttons(ctx, memory);
    }
});

// The menus' zones: Com_LoadUiFastFile's one DB_LoadXAssets call gets each installed
// map's picture after them, while the loader has room. (The loader copies the list's
// names, 64 bytes each, before it returns.)
guest_hook!(T_Com_LoadUiFastFile, |ctx, memory| {
    LOADING_MENUS.set(true);
    T_Com_LoadUiFastFile.original(ctx, memory);
    LOADING_MENUS.set(false);
});

// The pictures' zones written below the stack: how many, and where the list is.
fn write_previews(memory: Memory, sp: u32, zones: u32, count: u32, menus: u32, loaded: u32) -> (u32, u32) {
    let mut previews: Vec<Vec<u8>> = Vec::new();
    let mut found = lock(&PREVIEWS);
    found.clear();
    for file in arena::installed(&kernel::game_root()) {
        let name = file_stem(&file);
        let zone = [name.as_slice(), b"_preview"].concat();
        let zone_file = file.file_stem().map(|stem| arena::beside(&file, stem, "_preview.ff"));
        if zone.len() >= ZONE_NAME_SIZE as usize || !zone_file.is_some_and(|zone_file| zone_file.is_file()) {
            continue;
        }
        if u64::from(loaded.wrapping_add(count)) + previews.len() as u64 >= u64::from(MOST_ZONES) {
            log_warn!("maps: no room among the game's zones for %s's picture", shown(&name).as_c_str());
            continue;
        }
        previews.push(zone);
        for map in arena::maps(&arena::read(&file)) {
            found.entry(map.map).or_insert_with(|| [b"preview_".as_slice(), &name].concat());
        }
    }
    drop(found);
    let n = previews.len() as u32;
    if n == 0 {
        return (0, 0);
    }

    // The title's list on the stack, and the pictures' after it, each loaded and
    // freed as the menus are; their names below.
    let total = count.wrapping_add(n);
    let names = sp.wrapping_sub(n.wrapping_mul(ZONE_NAME_SIZE)).wrapping_sub(64) & !15;
    let list = names.wrapping_sub(total.wrapping_mul(ZONE_INFO_SIZE)) & !15;
    let flags = memory.u32_at(menus.wrapping_add(4));
    memory.copy(list, zones, count.wrapping_mul(ZONE_INFO_SIZE));
    for (i, zone) in previews.iter().enumerate() {
        let i = i as u32;
        let at = list.wrapping_add(count.wrapping_add(i).wrapping_mul(ZONE_INFO_SIZE));
        let name = names.wrapping_add(i * ZONE_NAME_SIZE);
        memory.zero(name, ZONE_NAME_SIZE);
        memory.store_bytes(name, zone);
        memory.store_u32(at, name);
        memory.store_u32(at.wrapping_add(4), flags);
        memory.store_u32(at.wrapping_add(8), 0);
    }
    (n, list)
}

guest_hook!(T_DB_LoadXAssets, |ctx, memory| {
    let custom_maps = *CUSTOM_MAPS;
    let zones = ctx.r3.as_u32();
    let count = ctx.r4.as_u32();
    let mut menus = 0; // ui_mp's entry
    let mut i = 0;
    while i < count && LOADING_MENUS.get() && menus == 0 {
        let entry = zones.wrapping_add(i.wrapping_mul(ZONE_INFO_SIZE));
        if equals(memory, memory.u32_at(entry), b"ui_mp") {
            menus = entry;
        }
        i += 1;
    }
    if !custom_maps || menus == 0 {
        T_DB_LoadXAssets.original(ctx, memory);
        return;
    }

    let loaded = memory.u32_at(T_DATA_DB_ZoneCount);
    let (n, list) = write_previews(memory, ctx.r1.as_u32(), zones, count, menus, loaded);
    if n == 0 {
        T_DB_LoadXAssets.original(ctx, memory);
        return;
    }
    let result = preserving(ctx, |ctx| {
        ctx.r1.u64 = u64::from(list.wrapping_sub(128));
        ctx.r3.u64 = u64::from(list);
        ctx.r4.u64 = u64::from(count.wrapping_add(n));
        T_DB_LoadXAssets.original(ctx, memory);
        ctx.r3.as_u64()
    });
    ctx.r3.u64 = result;
    log_info!(
        "maps: %u custom map picture%s with the menus' %u zones, %u of the loader's %u loaded before them",
        n,
        plural(n),
        count,
        loaded,
        MOST_ZONES
    );
});

// A custom map's longname and description are no key of the localize assets: the "@"
// lookup the menus show them through finds nothing, and the menus mark that
// "UNLOCALIZED(...)". They translate to themselves. A key the game has keeps its own
// translation.
guest_hook!(T_SEH_StringEd_GetString, |ctx, memory| {
    let key = ctx.r3.as_u32();
    T_SEH_StringEd_GetString.original(ctx, memory);
    if ctx.r3.as_u32() != 0 || key == 0 {
        return;
    }
    let Some(texts) = TEXTS.get() else { return };
    if texts.iter().any(|own| !own.contains(&0) && equals(memory, key, own)) {
        ctx.r3.u64 = u64::from(key);
    }
});
