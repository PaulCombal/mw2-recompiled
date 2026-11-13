# CI and owner-authorized automation

`mw3-validation.yml` runs on pushes, pull requests and manual dispatch on
x86-64 Ubuntu 24.04. All jobs have read-only repository permissions and
checkouts do not retain Git credentials. No job receives game inputs or API
secrets. Validation covers tooling, title routing, the runtime guard, three
asset-free C++ unit tests and the relay's locked Rust tests.

Run the standalone tests with:

```sh
cmake -S . -B build-tests -G Ninja -DMW2_TESTS_ONLY=ON -DCMAKE_BUILD_TYPE=Debug
cmake --build build-tests --parallel 2
ctest --test-dir build-tests --output-on-failure
```

The tests-only configuration returns before generated source discovery and
external dependency downloads. It needs CMake >=3.20, Ninja and a C++20
compiler. Python tooling needs Python >=3.11 for tomllib. Relay tests need a
Rust toolchain supporting edition 2024 and Cargo.lock's dependencies.

## Private builds

Automatic private-input builds have been removed from PR validation. The
previous workflow could expose private inputs to branch code and printed raw
runtime/audit output. Do not restore private builds until a protected manual
workflow, owner review of the exact source SHA and isolated execution exist.
The inherited release workflow is separate, publishes MW2-derived outputs,
and must not be used for this MW3 validation path.

A private build needs the supported XEX at mw3/tu0/default.xex, Clang 18,
LLD 18, CMake, Ninja, Python, Git, binutils, OpenSSL and CA certificates.
Install libvulkan-dev for renderer compile coverage. The recompiler script
fetches its pinned XenonRecomp revision/submodules and applies the local patch;
diagnostic linking fetches the checksum-pinned FFmpeg/XMA archive. A startup
run additionally needs the owner's complete extracted campaign game root.

Supply legally obtained inputs through owner-controlled private storage with
short-lived acquisition authorization. Keep acquisition credentials outside
build execution. Pin and review source and input revisions, restrict build
egress, disable core dumps, capture raw diagnostics only inside an ephemeral
private environment, and publish only fixed pass/fail results. Never cache or
upload inputs, generated code, binaries, raw logs or unaudited audit reports.
Do not execute unreviewed PR code with private inputs. A clean environment
alone does not prevent malicious code from exfiltrating its input files.

## Coding-agent boundary

Repository CI is automatic; unattended agent editing is not enabled here.
It requires an owner-controlled dispatch/broker that binds an approved task
to an immutable base SHA and explicit allowed scope. Issue text, labels and
comments are not authorization.

Keep the model API credential in an isolated service inaccessible to candidate
code and test processes. Run candidate tests in a separate environment with
no API credentials, private game inputs or write token. A trusted publisher
must validate the patch without executing it, reject changes outside scope,
and use a short-lived repository-scoped token to push an agent/* branch and
open a draft PR. It must never push to or merge into the default branch.
Protect that branch with required CI and owner review, without agent bypass.
Workflow changes require explicit owner authorization. Configuring those
services, credentials and repository protections is an owner administration
step; this repository does not contain or request API keys.
