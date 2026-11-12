# MW3 DPC import investigation

Status: research only. No DPC runtime handlers have been added.

## MW3 call-graph evidence

The latest metadata-only startup audit for the private MW3 XEX reports exactly one reachable direct callsite for each import:

- `KeInitializeDpc`: one callsite in generated function `sub_820A9670`.
- `KeInsertQueueDpc`: one callsite in generated function `sub_820A95A0`.

Shortest static paths from the XEX entry point (0x82459E58):

- Initialize: `__imp___xstart -> sub_8232E988 -> sub_8229DF30 -> sub_8229DA48 -> sub_823D7470 -> sub_823D71A0 -> sub_82090510 -> sub_820A6788 -> sub_820A9670 -> KeInitializeDpc`
- Insert: `__imp___xstart -> sub_8232E988 -> sub_8229E3F8 -> sub_823D7D70 -> sub_8209CDA0 -> sub_8209C898 -> sub_8209C588 -> sub_820A95A0 -> KeInsertQueueDpc`

This establishes startup reachability and one direct static callsite apiece. It does not establish runtime execution order, the guest addresses passed as arguments, the DPC's callback target, or what work the callback performs. The two callers are nearby in the MW3 image, but proximity alone does not prove they belong to the same subsystem.

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

## Decision

Do not implement these imports yet. The API-level contract is clear enough to define tests, but the Xbox-specific object layout and MW3's actual callback use are not yet proven. A guessed structure size or a synchronous callback shortcut risks memory corruption, wrong callback arguments, or startup deadlock.

## Required evidence before implementation

1. Obtain authoritative Xbox 360 KDPC layout evidence (XDK headers, symbols, validated emulator implementation, or direct guest access patterns); reconcile the 0x18 versus 0x1C discrepancy in the tentative Xenia representation.
2. Inspect the generated MW3 caller bodies at `sub_820A9670` and `sub_820A95A0` in CI without publishing generated proprietary source. Emit a sanitized report of the values/provenance assigned to r3-r5 at each import call and any subsequent accesses to the DPC object.
3. Resolve the callback address and inspect its direct/indirect call graph. Determine whether it signals an event, schedules work, touches graphics/audio state, or depends on a particular thread/IRQL.
4. Design a real queued-dispatch mechanism with duplicate suppression, argument capture, callback lifetime, guest stack/KPCR/TLS, synchronization, shutdown behavior, and a defined dispatch boundary.
5. Add synthetic tests for initialization, invalid callback handling, queue success, duplicate insertion, argument delivery, requeue during callback, and safe callback failure. Only then wire the handlers and run CI.

## References

- Microsoft: [KeInitializeDpc](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/wdm/nf-wdm-keinitializedpc)
- Microsoft: [KeInsertQueueDpc](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/wdm/nf-wdm-keinsertqueuedpc)
- Microsoft: [Registering and Queuing a CustomDpc Routine](https://learn.microsoft.com/en-us/windows-hardware/drivers/kernel/registering-and-queuing-a-customdpc-routine)
- Xenia Xbox kernel threading source (tentative KDPC notes and unimplemented dispatch): https://github.com/xenia-project/xenia/blob/master/src/xenia/kernel/xboxkrnl/xboxkrnl_threading.cc
- Xbox export ordinal list: https://github.com/XboxDev/xbedump/blob/master/xboxkrnl.h
- Xbox native port report on inline DPC impersonation: https://github.com/CrownParkComputing/Xbox360-Native-Ports/blob/main/docs/REXGLUE_FORK_CHANGES.md
