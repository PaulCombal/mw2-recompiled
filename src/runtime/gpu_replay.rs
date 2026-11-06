// MW2_REPLAY_GPU=<dir>: a capture MW2_CAPTURE_GPU took (runtime/gpu/stream_capture.h),
// fed back to the command processor with no title running, so that two builds, or
// one build twice, can be compared on the same GPU input (tools/replay_diff.py).
//
// The run (run.rs hands over before anything of the title's): guest memory and the
// physical heap's top as the capture had them, the renderer and its window, the
// snapshot and the command processor's state, the ring; then each batch, once the
// consumer has caught up -- its journal handed to the command processor's hook, the
// pages it found at the batch's start written, its ring dwords and the write pointer
// -- and at the end the reports. The title's interrupt callback is never set: the
// interrupts the stream raises go nowhere, as the C++ says once.

use crate::log::{log_error, log_info, log_warn};
use crate::runtime::bridge::{self, replay, run as renderer};
use crate::runtime::guest_memory::{self, PHYSICAL_BASE};
use crate::runtime::{crash, kernel, platform};
use std::ffi::CStr;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

const PAGE: usize = 4096;
// Where the title stores the ring's write pointer, big-endian (gpu/internal.h's kCpRbWptr).
const WRITE_POINTER: u32 = 0x7FC8_0714;
const FORMAT_VERSION: u32 = 2;

struct Batch {
    ring_at: usize, // in dwords, into ring.bin
    ring_dwords: u32,
    pages: Vec<(u32, u32, bool)>, // packet, page, blob, own
    blobs: Vec<u32>,
    events: Vec<replay::Event>,
}

struct Capture {
    ring_base: u32, // physical
    ring_dwords: u32,
    write_back: u32, // physical, or 0
    top: u32,
    snapshot: Vec<(u32, u32)>, // page, blob
    batches: Vec<Batch>,
    ring: Vec<u8>,
    blobs: Vec<u8>,
    state: Vec<u8>,
}

fn hex(text: Option<&str>) -> Result<u32, String> {
    let text = text.ok_or("a line ends early")?;
    u32::from_str_radix(text, 16).map_err(|_| format!("{text} is not hexadecimal"))
}

fn decimal(text: Option<&str>) -> Result<u64, String> {
    let text = text.ok_or("a line ends early")?;
    text.parse().map_err(|_| format!("{text} is not a number"))
}

fn read_file(folder: &Path, name: &str) -> Result<Vec<u8>, String> {
    std::fs::read(folder.join(name)).map_err(|error| format!("{}: {error}", folder.join(name).display()))
}

fn read_capture(folder: &Path) -> Result<Capture, String> {
    let index = String::from_utf8(read_file(folder, "capture.idx")?).map_err(|_| "capture.idx is not text")?;
    let mut lines = index.lines();
    let mut next = || lines.next().ok_or_else(|| "capture.idx ends early".to_string());
    let first = next()?;
    if first != format!("mw2 gpu capture {FORMAT_VERSION}") {
        return Err(format!("capture.idx begins \"{first}\", not format {FORMAT_VERSION}"));
    }
    let mut capture = Capture {
        ring_base: 0,
        ring_dwords: 0,
        write_back: 0,
        top: 0,
        snapshot: Vec::new(),
        batches: Vec::new(),
        ring: read_file(folder, "ring.bin")?,
        blobs: read_file(folder, "pages.blob")?,
        state: read_file(folder, "state.bin")?,
    };
    let blob_count = (capture.blobs.len() / PAGE) as u64;
    let mut ring_at = 0usize;
    let mut ended = false;
    while let Some(line) = lines.next() {
        let mut words = line.split(' ');
        match words.next() {
            Some("after") => {}
            Some("ring") => {
                capture.ring_base = hex(words.next())?;
                capture.ring_dwords = decimal(words.next())? as u32;
            }
            Some("writeback") => capture.write_back = hex(words.next())?,
            Some("top") => capture.top = hex(words.next())?,
            Some("snapshot") => {
                for _ in 0..decimal(words.next())? {
                    let line = lines.next().ok_or("the snapshot ends early")?;
                    let mut words = line.split(' ');
                    let page = hex(words.next())?;
                    let blob = decimal(words.next())?;
                    if blob >= blob_count {
                        return Err(format!("the snapshot names blob {blob} of {blob_count}"));
                    }
                    capture.snapshot.push((page, blob as u32));
                }
            }
            Some("batch") => {
                let _number = decimal(words.next())?;
                words.next();
                let ring_dwords = decimal(words.next())? as u32;
                words.next();
                let pages = decimal(words.next())?;
                words.next();
                let events = decimal(words.next())?;
                let mut batch =
                    Batch { ring_at, ring_dwords, pages: Vec::new(), blobs: Vec::new(), events: Vec::new() };
                ring_at += ring_dwords as usize;
                for _ in 0..pages + events {
                    let line = lines.next().ok_or("a batch ends early")?;
                    let mut words = line.split(' ');
                    let kind = words.next().unwrap_or("");
                    let kind = match kind {
                        "pool" => replay::POOL,
                        "wait" => replay::WAIT,
                        "aperture" => replay::APERTURE,
                        "arena" => replay::ARENA,
                        _ => {
                            let packet = kind.parse::<u32>().map_err(|_| format!("\"{line}\" is no journal line"))?;
                            let page = hex(words.next())?;
                            let blob = decimal(words.next())?;
                            if blob >= blob_count {
                                return Err(format!("a batch names blob {blob} of {blob_count}"));
                            }
                            batch.pages.push((packet, page, words.next() == Some("own")));
                            batch.blobs.push(blob as u32);
                            continue;
                        }
                    };
                    let mut event =
                        replay::Event { kind, packet: decimal(words.next())? as u32, ..replay::Event::default() };
                    match event.kind {
                        replay::POOL => {
                            event.a = hex(words.next())?;
                            event.b = hex(words.next())?;
                            event.c = hex(words.next())?;
                            event.d = hex(words.next())?;
                            event.e = decimal(words.next())?;
                        }
                        replay::WAIT => {
                            event.a = decimal(words.next())? as u32;
                            event.b = hex(words.next())?;
                            event.c = hex(words.next())?;
                            event.d = decimal(words.next())? as u32;
                            event.e = decimal(words.next())?;
                        }
                        replay::APERTURE => {
                            event.a = hex(words.next())?;
                            event.b = hex(words.next())?;
                        }
                        _ => event.a = hex(words.next())?,
                    }
                    batch.events.push(event);
                }
                capture.batches.push(batch);
            }
            Some("end") => ended = true,
            _ => return Err(format!("capture.idx has a line \"{line}\"")),
        }
    }
    if !ended {
        return Err("capture.idx has no end: the capture was not finished".into());
    }
    if ring_at * 4 != capture.ring.len() {
        return Err(format!("ring.bin holds {} bytes; the batches name {}", capture.ring.len(), ring_at * 4));
    }
    if !capture.ring_dwords.is_power_of_two() || capture.ring_dwords < 2 {
        return Err(format!("a ring of {} dwords", capture.ring_dwords));
    }
    Ok(capture)
}

// The physical heap's address of a physical one, as kernel::FromPhysical takes it.
fn from_physical(capture: &Capture, physical: u32) -> u32 {
    if physical < capture.top.wrapping_sub(PHYSICAL_BASE) { PHYSICAL_BASE + physical } else { 0 }
}

/// The replay, in place of the title's run: the exit code if it cannot start. A replay
/// that runs ends the process from request_exit, once the reports are written.
pub fn run(dir: &CStr) -> i32 {
    let folder = platform::system_path(dir);
    let capture = match read_capture(&folder) {
        Ok(capture) => capture,
        Err(why) => {
            log_error!("replay: %s", crate::log::c_string(why).as_c_str());
            return 1;
        }
    };
    let pages: usize = capture.batches.iter().map(|batch| batch.pages.len()).sum();
    let events: usize = capture.batches.iter().map(|batch| batch.events.len()).sum();
    log_info!(
        "replay: %s: %llu batches, %llu ring dwords, %llu snapshot pages, %llu page versions, %llu events",
        crate::log::c_string(folder.display().to_string()).as_c_str(),
        capture.batches.len() as u64,
        (capture.ring.len() / 4) as u64,
        capture.snapshot.len() as u64,
        pages as u64,
        events as u64
    );

    // Steps 1 to 3: no title; memory, the physical heap's top, the renderer, the window.
    // The ring's address is checked first: past gpu_initialise a failure leaves by
    // exit_now, as the renderer's threads would make a return's exit abort.
    let ring = from_physical(&capture, capture.ring_base);
    if ring == 0 {
        log_error!("replay: the ring at %08X is above the physical heap's top %08X", capture.ring_base, capture.top);
        return 1;
    }
    crash::install();
    if guest_memory::initialise().is_null() {
        return 1;
    }
    kernel::memory::set_physical_top(capture.top);
    crash::register_thread(c"main thread");
    renderer::gpu_initialise();
    renderer::show_window();

    // Step 4: the snapshot, and the command processor as the capture found it.
    let memory = guest_memory::memory();
    let blob = |id: u32| &capture.blobs[id as usize * PAGE..id as usize * PAGE + PAGE];
    for &(page, id) in &capture.snapshot {
        memory.store_bytes(page << 12, blob(id));
    }
    if !replay::restore(&capture.state) {
        log_error!("replay: state.bin is not the command processor's state as this build writes it");
        platform::exit_now(1);
    }
    replay::begin();

    // Step 5: the ring, read from its first dword.
    memory.store_u32(WRITE_POINTER, 0);
    if capture.write_back != 0 {
        bridge::gpu_set_read_pointer_write_back(capture.write_back, 0);
    }
    bridge::gpu_set_ring_buffer(capture.ring_base, (capture.ring_dwords * 4).trailing_zeros() - 3);

    // Step 6: batch by batch.
    let mask = capture.ring_dwords - 1;
    let mut written = 0u32;
    let mut timeouts = 0u64;
    // Every batch's journal, made before the first is handed over and kept to the end
    // of the run: a batch the consumer has not finished in 30 s still reads its own.
    let journal_pages: Vec<Vec<replay::Page>> = capture
        .batches
        .iter()
        .map(|batch| {
            batch
                .pages
                .iter()
                .zip(&batch.blobs)
                .map(|(&(packet, page, own), &id)| replay::Page {
                    packet,
                    page,
                    own: u32::from(own),
                    reserved: 0,
                    bytes: blob(id).as_ptr(),
                })
                .collect()
        })
        .collect();
    let journals: Vec<replay::Batch> = capture
        .batches
        .iter()
        .zip(&journal_pages)
        .map(|(batch, pages)| replay::Batch {
            pages: pages.as_ptr(),
            page_count: pages.len() as u64,
            events: batch.events.as_ptr(),
            event_count: batch.events.len() as u64,
        })
        .collect();
    let began = Instant::now();
    for (number, (batch, journal)) in capture.batches.iter().zip(&journals).enumerate() {
        // SAFETY: the journal, its pages and the blobs and events they point at live
        // in `capture`, `journal_pages` and `journals`, which this function holds until
        // the process ends: it never returns once the batches are fed.
        unsafe { replay::hand(journal) };
        // The pages the batch found at its start: through the plain address, so that
        // the write watch takes the faults the title's writes took.
        for (&(packet, page, own), &id) in batch.pages.iter().zip(&batch.blobs) {
            if packet == 0 && !own {
                memory.store_bytes(page << 12, blob(id));
            }
        }
        let dwords = &capture.ring[batch.ring_at * 4..(batch.ring_at + batch.ring_dwords as usize) * 4];
        for (i, dword) in dwords.chunks_exact(4).enumerate() {
            let at = written.wrapping_add(i as u32) & mask;
            memory.store_bytes(ring + at * 4, dword);
        }
        written = written.wrapping_add(batch.ring_dwords) & mask;
        memory.store_u32(WRITE_POINTER, written);
        let asked = Instant::now();
        while replay::consumer_read() != written {
            if asked.elapsed() > Duration::from_secs(30) {
                timeouts += 1;
                log_warn!("replay: the consumer did not finish batch %llu in 30 s", number as u64);
                break;
            }
            thread::sleep(Duration::from_micros(50));
        }
        if timeouts != 0 {
            break;
        }
    }
    log_info!("replay: %llu batches fed in %.1f s", capture.batches.len() as u64, began.elapsed().as_secs_f64());

    // Step 8: two blanks for the last frame to reach the window, then the reports.
    thread::sleep(Duration::from_micros(33_334));
    replay::finished(timeouts);
    crate::runtime::run::request_exit(c"replay finished");
    loop {
        thread::sleep(Duration::from_secs(1));
    }
}
