# Audio

The console's sound is split in two. The mixer is XAudio2, statically linked
into the title and therefore recompiled guest code. What it needs from outside
is a **render driver**, which calls its mix callback every 256 samples at
48 kHz (5.333 ms) and takes the six channels it hands back, and the **XMA
decoder**, hardware that XAudio2 drives through memory-mapped registers for
every compressed voice. Both are in `src/runtime/kernel/`, with the kernel
imports that reach them: `audio.rs` and `xma.rs`. The C++ build keeps
`runtime/apu/` and `runtime/kernel/audio.cpp`.

## Render driver (`audio.rs`)

`XAudioRegisterRenderDriverClient` receives `{ callback, context }` and returns
a driver handle. A worker thread, with its own guest stack (1 MB) and thread
block like any guest thread, calls `callback(context)` on the 5.333 ms cadence,
starting 100 ms after registration so the title has finished setting up its
object. Inside the callback the title mixes and calls
`XAudioSubmitRenderDriverFrame` with six planes of 256 big-endian floats (FL FR
FC LFE BL BR). These are interleaved and pushed to an SDL3 audio stream opened
as 5.1 float at 48 kHz; SDL converts to whatever the device is.

The callback receives the context value itself, not a pointer to it: MW2's
callback tests a "started" flag inside that object and returns without mixing
when it is clear.

Pacing follows the playback device's clock, as the console's follows its audio
hardware's. The stream has a get callback, which SDL calls on its device thread
each time the device takes sound from the queue: it records how much the take
asks for and what the queue holds after it, and wakes the worker. The worker
then calls the title's callback until the queue holds that take again plus a
lead of three frames, about five frames (26 ms) with WASAPI's 480-sample
period, and follows the device if its period changes (another device, another
period). The worker answers a take by waking the title's mixer thread and
waiting for its mix, so the lead is how late those two threads may be
scheduled: under Wine with every processor busy, one frame underran about 15
times a minute and two never did. A call that submits a frame refills the queue
up to the target and no further; one that submits nothing is paid for by what
the device asked, a credit of its bytes capped at the target, so a title that
submitted nothing would still run at the device's rate. A take that finds the queue short is an underrun, and SDL plays
silence for the part that is missing. Frames are dropped once the queue holds a
take, the lead and 24 frames (128 ms) more, a guard for a device that stalls
with its queue full.

Without a device (`MW2_NO_AUDIO=1`, or none opened) the wall clock calls the
callback every 5.333 ms instead, because the title's sound state machines
advance inside it and a silent run must still finish its sounds; a worker that
falls far behind (the process was stopped) resynchronises instead of calling a
burst. The mixer and the decoder still run.

Two other imports matter to the mixer:

- `XAudioGetVoiceCategoryVolume` reports full volume, since there is no
  dashboard.
- `XAudioGetVoiceCategoryVolumeChangeMask` reports every category changed on its
  first call. The mixer only fetches a category's volume when told it changed,
  and its object starts zeroed, so otherwise every voice stays at volume 0.

The mixer also depends on kernel behaviour described in
[runtime.md](runtime.md): it creates its mixing thread suspended and starts it
with `KeResumeThread`; its workers wait for each other by processor number,
which comes from the top byte of `ExCreateThread`'s flags; its callback waits
on a "done" and a "stop" event with `KeWaitForMultipleObjects` and needs the
index back; and it reads `KeDebugMonitorData`, which must point at a zero word.

## XMA decoder (`xma.rs`)

A port of Xenia's context decoder (`xma_context_new.cc`): the same context
layout, the same packet and frame walk. XMA2 is WMA Pro in 2 KB packets, each
starting with a 32-bit header and holding 15-bit-length-prefixed frames of 512
samples that may straddle packets. The context records a bit offset into its
input buffer, and XAudio2 loops a sound by having the decoder jump from the loop
end back to the loop start.

**Contexts.** 320 contexts of 64 bytes are allocated from the physical heap
before the title starts (not at physical address 0, which reads as "no
context"), since the title finds a context's hardware index from its physical
address. `XMACreateContext` and `XMAReleaseContext` hand them out.

**Registers.** The register window at `0x7FEA0000` is plain guest memory.

| offset | direction | contents |
|---|---|---|
| `+0x1800` | read | physical address of context 0 |
| `+0x1818` | read | the "current context", preset to one no context has, so the title's lock-all never sees a busy context |
| `+0x1940` | write | kick: ten words, one bit per context |
| `+0x1A40` | write | lock |
| `+0x1A80` | write | clear |

The written registers are commands, not state, so they are caught as they are
stored. The recompiler emits `PPC_MM_STORE_U32` for a store followed by
`eieio`, and `runtime/mmio_hook.h`, force-included into the recompiled code,
routes stores into the window to `mw2_xma_register_store`. A kick decodes
synchronously on the kicking thread until the output ring is full or the input
is spent, so the context reflects the work when the store returns. A lock waits
out a decode in flight on another thread.

**Frames** are decoded by `src/runtime/wmapro/`, a Rust port of the `xmaframes`
codec of Xenia's FFmpeg branch (`has207/FFmpeg`): libavcodec's WMA Pro decoder
fed one XMA frame at a time. Stock FFmpeg has only a packet-level XMA2 decoder,
which cannot follow the frame-precise bit offsets and loop points a context
works in. The port does what the C does in the same order, down to the bit
reader, the VLC tables, and the inverse MDCT and FFT of libavutil's C (this
build of FFmpeg has no assembly), so it gives the same floats bit for bit, logs
what FFmpeg logged and refuses what it refused; it is LGPL 2.1 or later, as
FFmpeg is (`src/runtime/wmapro/NOTICE.md`). Each context makes a decoder for
its stream's rate and channels, and a new one when they change; a 512-sample
frame comes out as planar float, which the context turns into 16-bit
big-endian PCM.

The C++ build still decodes with FFmpeg itself, fetched as a tarball at
configure time and built as the subset in `third_party/ffmpeg-xenia/`, with
Xenia's hand-written `config.h`; the Rust build makes that library only for
`tools/xma_diff.py`. A context released and given to another voice keeps the
decoder's overlap from the last one, on both builds: FFmpeg's `xmaframes` has
no flush.

**Damaged streams.** On some input the title never gives, the decoder, Xenia's
included, never returns from a kick: a loop for ever over frames that all fail
to decode, or a packet header that sends the read offset back over a frame that
fails. The C++ also copies out of bounds on some: a frame length that runs past
the two packets it has, a ring of zero blocks, a context turned mono with a
stereo frame in flight. The Rust stops at the buffer's end, takes a ring of
zero blocks as full and writes silence for the blocks a frame does not have.

## Diagnostics

The end of a run gives, for the driver, callbacks, frames submitted, drops,
underruns and what paces it, in a player's build too (both builds); the
end-of-run report (diagnostic builds) gives, for the
decoder, kicks, locks, clears, frames decoded, the contexts used and the peak
allocated, and errors.

| switch | effect |
|---|---|
| `MW2_NO_AUDIO=1` | no playback device |
| `MW2_TRACE_XMA=1` | every kick with its buffers (the C++ build adds FFmpeg's whole log) |
| `MW2_XMA_RECORD=<file>` | write down every context given out, released, cleared and kicked, with the input each kick sees, for `tools/xma_diff.py` |

`tools/xma_diff.py <build folder> <recording>...` replays recordings through
the C++ decoder and the Rust in one program, each on a guest space of its own,
and compares the context block, the whole output ring and the log after every
kick; `--fuzz N` also damages some kicks the same way on both sides (fields of
the block, bits of the input, stores to registers the title never writes),
after trying each on the C++ alone in a child, which leaves out what it would
hang or fault on, and runs it on the Rust alone, which must not fault.
`--coverage` names the lines of `xma.cpp` nothing reached. Its Rust is built
with `--cfg mw2_xma_rig`, which decodes every frame with FFmpeg too, through
`runtime/apu/xma_codec.cpp`, and logs where the two differ by a bit: the C++
side logs no such line, so any difference fails the replay.

To check the output without listening, record the sink's monitor while the game
runs:

    parec --device="$(pactl get-default-sink).monitor" --file-format=wav out.wav
