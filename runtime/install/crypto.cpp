#include "crypto.h"

#include <cstring>

// AES-128 decryption (FIPS 197, the equivalent inverse cipher written out
// round by round). Only an XEX's 5 MB goes through it, once per launch, so it
// is table-free apart from the S-boxes, which are derived rather than typed.
namespace
{
    struct Boxes
    {
        uint8_t sbox[256];
        uint8_t inverse[256];
        Boxes()
        {
            // The S-box is the multiplicative inverse in GF(2^8) followed by an
            // affine map; walking p and q = 1/p together yields both at once.
            uint8_t p = 1, q = 1;
            do
            {
                p = uint8_t(p ^ (p << 1) ^ (p & 0x80 ? 0x1B : 0));   // p *= 3
                q ^= q << 1; q ^= q << 2; q ^= q << 4;               // q /= 3
                if (q & 0x80) q ^= 0x09;
                auto rotl = [](uint8_t x, int s) { return uint8_t((x << s) | (x >> (8 - s))); };
                const uint8_t s = uint8_t(q ^ rotl(q, 1) ^ rotl(q, 2) ^ rotl(q, 3) ^ rotl(q, 4) ^ 0x63);
                sbox[p] = s;
            } while (p != 1);
            sbox[0] = 0x63;
            for (int i = 0; i < 256; i++) inverse[sbox[i]] = uint8_t(i);
        }
    };
    const Boxes g_boxes;

    uint8_t Times2(uint8_t x) { return uint8_t((x << 1) ^ (x & 0x80 ? 0x1B : 0)); }
    uint8_t Multiply(uint8_t x, uint8_t y)
    {
        uint8_t r = 0;
        for (; y; y >>= 1, x = Times2(x))
            if (y & 1) r ^= x;
        return r;
    }

    // The eleven round keys, 16 bytes each.
    void Expand(const install::crypto::Key& key, uint8_t out[176])
    {
        std::memcpy(out, key.data(), 16);
        uint8_t rcon = 1;
        for (int i = 16; i < 176; i += 4)
        {
            uint8_t t[4] = { out[i - 4], out[i - 3], out[i - 2], out[i - 1] };
            if (i % 16 == 0)
            {
                const uint8_t first = t[0];
                t[0] = uint8_t(g_boxes.sbox[t[1]] ^ rcon);
                t[1] = g_boxes.sbox[t[2]];
                t[2] = g_boxes.sbox[t[3]];
                t[3] = g_boxes.sbox[first];
                rcon = Times2(rcon);
            }
            for (int j = 0; j < 4; j++) out[i + j] = uint8_t(out[i - 16 + j] ^ t[j]);
        }
    }

    void DecryptBlock(const uint8_t keys[176], uint8_t s[16])
    {
        for (int i = 0; i < 16; i++) s[i] ^= keys[160 + i];
        for (int round = 9; ; round--)
        {
            // Inverse ShiftRows: row r (bytes r, r+4, r+8, r+12) rotates right by r.
            uint8_t t[16];
            for (int c = 0; c < 4; c++)
                for (int r = 0; r < 4; r++) t[((c + r) % 4) * 4 + r] = s[c * 4 + r];
            for (int i = 0; i < 16; i++) s[i] = uint8_t(g_boxes.inverse[t[i]] ^ keys[round * 16 + i]);
            if (round == 0) break;
            for (int c = 0; c < 4; c++)   // inverse MixColumns
            {
                uint8_t* col = s + c * 4;
                const uint8_t a0 = col[0], a1 = col[1], a2 = col[2], a3 = col[3];
                col[0] = uint8_t(Multiply(a0, 14) ^ Multiply(a1, 11) ^ Multiply(a2, 13) ^ Multiply(a3, 9));
                col[1] = uint8_t(Multiply(a0, 9) ^ Multiply(a1, 14) ^ Multiply(a2, 11) ^ Multiply(a3, 13));
                col[2] = uint8_t(Multiply(a0, 13) ^ Multiply(a1, 9) ^ Multiply(a2, 14) ^ Multiply(a3, 11));
                col[3] = uint8_t(Multiply(a0, 11) ^ Multiply(a1, 13) ^ Multiply(a2, 9) ^ Multiply(a3, 14));
            }
        }
    }
}

void install::crypto::Aes128CbcDecrypt(const Key& key, uint8_t* data, size_t size)
{
    uint8_t keys[176];
    Expand(key, keys);
    uint8_t previous[16] = {};
    for (size_t offset = 0; offset + 16 <= size; offset += 16)
    {
        uint8_t* block = data + offset;
        uint8_t cipher[16];
        std::memcpy(cipher, block, 16);
        DecryptBlock(keys, block);
        for (int i = 0; i < 16; i++) block[i] ^= previous[i];
        std::memcpy(previous, cipher, 16);
    }
}
