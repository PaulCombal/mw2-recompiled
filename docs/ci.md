# CI and owner-authorized automation

`mw3-validation.yml` runs on pushes, pull requests and manual dispatch on
x86-64 Ubuntu 24.04. All jobs have read-only repository permissions and
checkouts do not retain Git credentials. No job receives game inputs or API
secrets. Validation covers tooling, title routing, the runtime guard, six
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

## Game data stays local

CI never receives game files, and none are stored anywhere remote: no private
asset repository, no Git LFS, no cache and no artifact upload. The earlier
private-input workflow was removed and must not be restored in any form.
`tools/check_no_game_files.py` fails CI if a game file or an LFS pointer is
tracked.

Anything that needs the executable or game data runs only on the owner's own
machine ([MW3-LOCAL-RUN.md](MW3-LOCAL-RUN.md)). That needs the supported XEX at
`mw3/tu0/default.xex`, Clang 18, LLD 18, CMake, Ninja, Python, Git, binutils,
OpenSSL and CA certificates, plus libvulkan-dev for renderer compile coverage.
The recompiler script fetches its pinned XenonRecomp revision/submodules and
applies the local patch; diagnostic linking fetches the checksum-pinned
FFmpeg/XMA archive. A startup run additionally needs the owner's complete
extracted campaign game root. Never upload inputs, generated code, binaries,
raw logs or unaudited audit reports. The inherited release workflow is separate,
publishes MW2-derived outputs, and must not be used for this MW3 path.

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
