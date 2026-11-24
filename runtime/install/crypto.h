#pragma once
// AES-128, to decrypt an XEX: what the compile pass and the tools need to read the
// game's executable (xex.h). What knows a file by its SHA-256 is Rust, the install
// crate's (install/src/crypto.rs).
#include <array>
#include <cstddef>
#include <cstdint>

namespace install::crypto
{
    using Key = std::array<uint8_t, 16>;

    // CBC with a zero IV, in place; `size` is a multiple of 16.
    void Aes128CbcDecrypt(const Key& key, uint8_t* data, size_t size);
}
