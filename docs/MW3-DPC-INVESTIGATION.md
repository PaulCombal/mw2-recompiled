# MW3 DPC import investigation

Status: first implementation added (`runtime/kernel/dpc.cpp`, `dpc_queue.h`, `tests/dpc_queue_test.cpp`), unvalidated on a live boot. The research below is unchanged and still lists what remains unproven.

**What was built.** The Xbox KDPC layout question is sidestepped: the title only passes the DPC pointer around, so the runtime keeps its own record per guest address and never reads or writes the KDPC's memory. `KeInitializeDpc` records routine and context. `KeInsertQueueDpc` returns 1 when it queues and 0 when the DPC is unknown or already queued, and clears the queued flag before the routine runs so the routine may requeue. Routines run as `routine(dpc, context, arg1, arg2)` on one dedicated worker with its own guest stack and thread block, one at a time and never inline in the queuing call. Unit tests cover duplicate insert, argument capture, FIFO order, requeue, re-initialise and wake/stop (ASan, UBSan and TSan clean).

**Not modelled:** IRQL, processor affinity, importance, `KeRemoveQueueDpc`, timer-to-DPC delivery, and the exact ordering against the graphics interrupt thread. Items 1 to 3 of "Required evidence" below remain open; if a boot shows the completion ring misbehaving, ordering against the GPU interrupt callback is the first thing to check.

## MW3 call-graph evidence

The latest metadata-only startup audit for the private MW3 XEX reports exactly one reachable direct callsite for each import:

- `KeInitializeDpc`: one callsite in generated function `sub_820A9670`.
- `KeInsertQueueDpc`: one callsite in generated function `sub_820A95A0`.

Shortest static paths from the XEX entry point (0x82459E58):

- Initialize: `__imp___xstart -> sub_8232E988 -> sub_8229DF30 -> sub_8229DA48 -> sub_823D7470 -> sub_823D71A0 -> sub_82090510 -> sub_820A6788 -> sub_820A9670 -> KeInitializeDpc`
- Insert: `__imp___xstart -> sub_8232E988 -> sub_8229E3F8 -> sub_823D7D70 -> sub_8209CDA0 -> sub_8209C898 -> sub_8209C588 -> sub_820A95A0 -> KeInsertQueueDpc`

This establishes startup reachability and one direct static callsite apiece. The callback target is now resolved below. The two imports are in different functions, but further object-offset analysis establishes a strong relationship between their DPC records.

## Recovered callback and DPC-array relationship

The generated-code CI artifacts for the validated private MW3 XEX were inspected locally. Proprietary generated PPC statements and XEX bytes are not committed or emitted in the report. The sanitized audit now verifies the key address/stride patterns.

### Callback

- `KeInitializeDpc` receives callback guest address `0x820A9568`, mapped to `sub_820A9568`, and a per-entry deferred-context pointer.
- The callback treats r4 as its deferred context and r5 as a completion value. It reads a counter at context offset `0x54`, writes the value into one of four 32-bit ring slots at offsets `0x5C` through `0x68`, increments the counter, and signals a `KeSetEvent` event at context offset `0x3C`.
- This is a completion/counter notification callback, not an arbitrary worker routine.
- The initializer also invokes a thread-creation helper with guest entry point `0x820A93D0` and the same per-entry context. The mapped worker `sub_820A93D0` waits on the event at context offset `0x3C` and calls `sub_820A9110` to consume completion records.
- The consumer compares producer and consumer counters at context offsets `0x54` and `0x58`, reads the same four-slot ring at `0x5C` through `0x68`, and advances the consumer counter. The callback is therefore a short DPC-level producer; the guest worker performs the downstream completion handling.

### Matching initialized and queued DPC records

The initialization and insertion paths resolve to the same parent object and same entry stride:

- `sub_820A6788` passes its object pointer to `sub_820A9670`. The initializer sets up six per-entry records, stepping by `0x6C` bytes.
- For entry `i`, its deferred context is `object_base + 0x2CC0 + i * 0x6C`, and its DPC object is `object_base + 0x2CE0 + i * 0x6C` (the DPC begins `0x20` bytes into that record).
- `sub_8209C588` passes the same parent object pointer to `sub_820A95A0`. The insertion function selects the entry from the index field at object offset `0x2CBC`, multiplies the index by `0x6C`, and computes the DPC address at object offset `0x2CE0`. This matches the initializer's DPC array formula.
- The initialization parent also calls `VdInitializeEngines` and `VdSetGraphicsInterruptCallback`. The insertion caller takes the object's spin lock before its conditional queue path.

**Interpretation:** the matching base, index, offset, and stride, together with video-engine initialization and graphics-interrupt callback registration, are strong evidence that these DPCs implement a graphics-interrupt/completion notification path. The callback's event and four-slot completion ring, plus the worker's producer/consumer loop, are consistent with that role. Exact GPU fence semantics, runtime ordering, and Xbox DPC scheduler behavior remain unproven.

The DPC object is therefore not shown to be stack-local: its address is derived from the persistent parent object and indexed record array. The earlier stack-local interpretation was incorrect; it confused saved register provenance with the actual guest object base.

## API contract supported by reference material

The conventional DPC contract is:

- Initialize a caller-owned DPC object with a deferred routine and deferred context.
- Queue it with two caller-supplied system arguments.
- A DPC object already queued cannot be queued again; insertion returns false. A newly queued object returns true.
- The deferred routine receives four arguments: DPC pointer, deferred context, system argument 1, and system argument 2.
- Execution is deferred to a kernel dispatch point; it is not equivalent to calling the routine inline as part of the insertion API.

Microsoft's DDI pages describe the Windows contract. Xbox 360's kernel is related to NT but is not identical, so this is a behavioral reference, not proof of every Xbox 360 detail.

## Xbox 360-specific evidence and uncertainty

The Xbox export ordinal lists identify `KeInitializeDpc` and `KeInsertQueueDpc` as Xbox kernel exports (ordinals 107 and 119 in the xbedump list; ordinal values vary by kernel build).

Xenia's Xbox kernel source contains a tentative Xbox DPC representation:
- a header word initialized with type value 19, importance 0, processor number 0;
- list links;
- routine and context pointers;
- two saved arguments.

However, Xenia itself labels the structure as tentative ("maybe"), comments that its size may be 0x18, while the displayed seven-word field declaration spans 0x1C bytes, and its `KeInsertQueueDpc` implementation deliberately asserts because dispatch is not implemented. This is useful reverse-engineering evidence, but not a sufficiently verified structure definition to copy into MW3.

A separate Xbox-native-port project reports that it fixed lost DPC-driven signals by running DPC callbacks inline under DPC impersonation. That demonstrates one practical compatibility strategy, not the Xbox scheduler's full semantics; inline dispatch can change ordering, reentrancy, and the timing of callbacks.

## Existing MW3 runtime constraints

The shared runtime currently has:
- `kernel::GuestFunction(address)`, which resolves a guest callback address to generated PPC code;
- `kernel::CallGuest(caller, address, a0, a1, a2)`, which invokes a guest callback synchronously, copying r13 and borrowing stack space below the caller's stack frame;
- guest-thread creation that creates a host thread, guest stack, and per-thread KPCR/TLS block;
- no general DPC queue, DPC worker, DPC object registry, or modeled IRQL/dispatch boundary.

Consequently, calling `CallGuest` directly inside `KeInsertQueueDpc` would be unsafe as a general implementation: it would execute synchronously, would not preserve the queued/not-queued state contract, and could re-enter guest code before the queuing call has returned. Moving it to a host worker also cannot simply reuse the original caller's `PPCContext` or borrowed stack; the callback needs a valid guest execution context and lifetime-safe guest stack/KPCR/TLS state.

The existing timer thread is not a DPC scheduler. Timer-to-DPC delivery would need to be integrated separately if MW3 later reaches `KeSetTimer` with a DPC object.

## Decision (superseded by the status above)

The original decision was not to implement these imports yet. The MW3 object relationship and callback role are now substantially better understood, but the Xbox-specific KDPC layout and dispatch contract are still not proven. A guessed structure size or synchronous callback shortcut risks memory corruption, wrong callback arguments, or startup deadlock.

## Required evidence before implementation

1. Obtain authoritative Xbox 360 KDPC layout evidence (XDK headers, symbols, validated emulator implementation, or direct guest access patterns); reconcile the 0x18 versus 0x1C discrepancy in the tentative Xenia representation.
2. Verify the six-entry record array, per-entry context/DPC offsets, and ring/event behavior against independent XEX disassembly or symbols where available; confirm whether all six entries are active on target hardware.
3. Trace the graphics interrupt producer and consumer further, including how the completion value passed as system argument 1 is generated and interpreted, and determine the required thread/IRQL/dispatch timing.
4. Design a real queued-dispatch mechanism with duplicate suppression, argument capture, callback lifetime, guest stack/KPCR/TLS, synchronization, shutdown behavior, and a defined dispatch boundary.
5. Add synthetic tests for initialization, invalid callback handling, queue success, duplicate insertion, argument delivery, requeue during callback, and safe callback failure. Only then wire the handlers and run CI.

## References

- Microsoft: [KeInitializeDpc](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/wdm/nf-wdm-keinitializedpc)
- Microsoft: [KeInsertQueueDpc](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/wdm/nf-wdm-keinsertqueuedpc)
- Microsoft: [Registering and Queuing a CustomDpc Routine](https://learn.microsoft.com/en-us/windows-hardware/drivers/kernel/registering-and-queuing-a-customdpc-routine)
- Xenia Xbox kernel threading source (tentative KDPC notes and unimplemented dispatch): https://github.com/xenia-project/xenia/blob/master/src/xenia/kernel/xboxkrnl/xboxkrnl_threading.cc
- Xbox export ordinal list: https://github.com/XboxDev/xbedump/blob/master/xboxkrnl.h
- Xbox native port report on inline DPC impersonation: https://github.com/CrownParkComputing/Xbox360-Native-Ports/blob/main/docs/REXGLUE_FORK_CHANGES.md
