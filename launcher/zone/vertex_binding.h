#pragma once
// A vertex shader as the game draws it.
//
// A vertex shader in a fastfile names no vertex format: its fetches read
// format 0. While the title loads a material pass it binds the pass's vertex
// shader for each of the 15 vertex declaration types to that declaration
// (IDirect3DVertexShader9::Bind), and D3D rewrites the shader's fetches in
// place -- fetch constant, format, offset and stride -- merging neighbours
// that read one stream into mini-fetches. That rewritten program is the one
// the command stream loads, and so the one to compile.
//
// The declaration comes from the pass's stream routing, through the title's
// tables of where each source is in each declaration type's streams
// (sub_823EA570 in the multiplayer), and D3D's CreateVertexDeclaration. All of
// it is reproduced here from the same data: the routing and the shader in
// the fastfile, the tables in the executable.
#include <array>
#include <cstdint>
#include <vector>

namespace zone
{
    // What the binding reads from the executable.
    struct BindTables
    {
        std::array<uint8_t, 15 * 72> types{};    // per declaration type: { stream, offset, format } per source
        std::array<uint8_t, 14 * 2> usages{};    // per destination: { usage, usage index }
        std::array<uint32_t, 15> strides[2]{};   // per declaration type, of streams 0 and 1, in bytes
        std::array<uint8_t, 16> swizzles{};      // D3D's: the components a fetch's format leaves
        std::array<uint8_t, 64> formats{};       // D3D's: the dwords a fetch of each format reads
    };
    // From an executable's memory image (at 0x82000000).
    BindTables ReadBindTables(const std::vector<uint8_t>& image, bool multiplayer);

    // The programs of `object` (a vertex shader object as a fastfile holds it:
    // the load def's cached part) in `program` (its physical part), bound for
    // declaration type `slot` of a pass with this declaration (the fastfile's
    // 64 bytes). False when the title binds nothing there: a source the type
    // has no stream for (sub_823C2E28 finds no declaration and skips Bind), or
    // slot 15, the pass's own vertexShader, which the pass loader reads with
    // the alias loader alone (sub_821BBC08), never through sub_821BBE70.
    bool Bind(const BindTables& tables, const uint8_t* declaration, unsigned slot, const std::vector<uint8_t>& object,
              std::vector<uint8_t>& program);

    // The image pool's memory move binds its shader at its draw (D3D binds a
    // shader the declaration set beside it has not bound), to the declaration
    // the title made for it, D3D's object of it.
    std::vector<uint8_t> MoveDeclaration(const std::vector<uint8_t>& image, bool multiplayer);
    // Its stream's stride, in dwords: T_Image_FlushMove sets the vertex
    // stream with 64 bytes (li r7,64 before the call), the 8 elements of 8.
    constexpr uint8_t kMoveStride = 64 / 4;

    // The binding itself: the programs of the shader whose header is at
    // `header` in `object`, for D3D's declaration object `declaration` with a
    // stride per stream in dwords.
    void BindPrograms(const BindTables& tables, const std::vector<uint8_t>& declaration, const uint8_t* strides,
                      const std::vector<uint8_t>& object, size_t header, std::vector<uint8_t>& program);
}
