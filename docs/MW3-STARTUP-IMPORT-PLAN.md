# MW3 startup import implementation plan

Status: triage plan based on the current `mw3-bringup` source and the sanitized startup/DPC audit documented in this repository. This document does not claim that any import has been dynamically observed during boot.

## Decision

Implement low-risk, independently testable RTL/C-runtime compatibility first. Keep graphics-completion DPC dispatch deferred until its Xbox-specific object layout, execution context, ordering, and lifetime requirements are verified. Do not replace diagnostic stubs with guessed zero-return handlers.

The current static audit reports 258 kernel/XAM imports in the image, 188 reachable through its direct-call graph, 161 reachable imports matching an existing runtime `PPC_FUNC` definition, and 27 reachable imports covered only by generated logging stubs. These are name-level coverage counts, not semantic compatibility results.

## Prioritized implementation queue

| Priority | Import(s) | Why it is in this tier | Required investigation before implementation | Acceptance tests |
|---|---|---|---|---|
| P0 | `RtlImageXexHeaderField` | The bring-up audit identifies a call from CRT startup at `sub_82462E50`. It is an early startup dependency and can be tested without graphics. | Recover the exact Xbox 360 signature, XEX header field identifiers, absent-field behavior, bounds rules, and guest-pointer representation. Compare with the XEX structures already parsed by `tools/xexinfo.py` / `tools/xexdump.py`; do not assume host PE semantics are identical. | Synthetic XEX headers covering present, absent, malformed, and out-of-range fields; guest-address translation; no out-of-bounds reads. |
| P1 | `sprintf`, `_snprintf`, `_vsnprintf`, `vswprintf` | The bring-up audit identifies these reachable formatters and reports use on paths including the D3D-present path. They are common dependencies and are separable from GPU work. | Confirm each Xbox CRT variant's return-value and truncation contract, termination behavior, wide-character encoding, supported format specifiers, and PPC varargs/va_list layout. A host libc call is not automatically ABI-compatible. | Golden tests for integer, pointer, string, width/precision, floating point if required, wide strings, zero-size buffers, truncation, return values, and canary-protected guest buffers. Include PPC varargs fixtures generated from guest calling convention. |
| P2 | `RtlUnicodeStringToAnsiString`, `RtlFreeAnsiString` | The bring-up audit identifies this allocation/conversion pair as reachable. Correct behavior depends on ownership and guest memory management. | Verify Xbox RTL structure layout, byte lengths versus character counts, destination allocation rules, `AllocateDestinationString`, null/empty input, conversion policy, and matching guest allocator/free routine. Confirm whether the MW3 callsites request allocation or supply storage. | Tests for allocated and caller-provided destination buffers, empty and non-ASCII input, length limits, allocation failure, ownership, and exactly-once free. |
| P3 | `VdSetDisplayModeOverride` | Reachable display-mode import; likely relevant to display initialization but requires title/display contract work. | Recover argument structure and expected side effects from callsites and Xbox references. Determine whether the MW2 runtime has a semantically equivalent display-mode path and whether MW3 uses the same mode fields. | Structure-layout tests, supported/unsupported mode tests, deterministic state update, and integration test proving the selected mode reaches the presenter. |
| Deferred / graphics-critical | `KeInitializeDpc`, `KeInsertQueueDpc` | One reachable direct callsite each; current investigation traces a six-entry DPC array, completion ring, event and worker. This is not a safe stub-to-handler conversion. | Verify Xbox KDPC size/alignment and field offsets, duplicate-queue behavior, callback argument mapping, dispatch context/IRQL, ordering, requeue semantics, synchronization, worker interaction, and shutdown/lifetime. Reconcile the tentative Xenia 0x18/0x1C discrepancy. | Synthetic DPC lifecycle tests: initialize, queue, duplicate insert, argument capture, callback requeue, concurrent queueing, callback failure, teardown with queued work, and graphics completion ordering. |
| Leave stubbed for offline campaign until evidence says otherwise | Network, `Xam*UI`, bug-check and unrelated service imports | Current bring-up notes classify these as nonessential to the initial offline campaign boot. | Revisit only when a concrete startup path, campaign path, or crash demonstrates a dependency. | Add targeted tests only when promoted into the required boot path. |

## Implementation rules

1. Work from exact MW3 callsites in the generated-code audit, not from import names alone. Static reachability can include conditional/error paths and misses indirect calls, function pointers, callbacks, data imports, and dynamically selected thread entry points.
2. Before implementing an import, record its guest call signature, register/stack argument locations, return convention, pointer ownership, error behavior, and relevant Xbox-specific evidence.
3. Prefer small handlers with focused unit tests. Keep host pointers and guest addresses distinct; all guest memory access must go through the runtime's guest-memory model.
4. Do not count an import as implemented merely because a `PPC_FUNC` macro exists. Update the audit classification only after behavioral tests establish the contract.
5. Regenerate the kernel stub file after each handler change and verify the import moves from generated logging stub to runtime implementation without duplicate definitions.
6. Preserve the DPC investigation as a separate design task. Do not call a guest DPC routine inline from `KeInsertQueueDpc`, and do not reuse a caller's PPC context or borrowed stack on an arbitrary host worker.

## Validation sequence for each import

- Run the import-specific unit tests and existing tool self-tests.
- Regenerate MW3 shared declarations and kernel stubs from the supported XEX.
- Run the startup call-graph audit and verify the expected import classification/count change.
- Build `mw3-recomp-check` and the generated PPC library.
- Run the existing MW2 build/tests to detect regressions in shared runtime code.
- Preserve a sanitized test/audit artifact; never commit the XEX, extracted game data, or proprietary generated PPC source.

## Current evidence and source locations

- [MW3 bring-up status and startup import shortlist](MW3-BRINGUP.md#progress)
- [DPC callback, object-array and dispatch investigation](MW3-DPC-INVESTIGATION.md)
- [Generated-code build scope and audit limitations](MW3-BUILD.md)
- [Startup direct-call audit implementation](../tools/audit_mw3_startup.py)
- [Kernel logging-stub generator](../tools/gen_kernel_stubs.py)
- [MW3 title-specific runtime configuration](../runtime/title.h)
- [MW3 compile-only CMake target](../CMakeLists.txt)

## Important audit limitation

The report's "implemented" classification is a regular-expression match for a runtime `PPC_FUNC(__imp__...)` definition. It does not inspect handler bodies, validate signatures, prove the Xbox contract, or establish that the handler works for MW3. The direct-call graph likewise does not establish runtime execution order. Treat all 161 existing matches as unverified until their semantics are exercised by the game or by suitable tests.
