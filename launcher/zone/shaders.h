#pragma once
// Every shader in a fastfile, as the game will hand it to the renderer.
//
// A material's techniques carry their shaders inline. Each is in two parts:
// the D3D shader object (the "cached" part) and the program the GPU runs (the
// "physical" part). When the game loads one, runtime/shader_preload.cpp reads
// the object's header to find where in the program the microcode is, and
// hands the renderer those bytes. The same rule over the same two parts in the
// file gives the same bytes, and so the same name (vk::renderer::ShaderHash).
#include "schema.h"

#include <filesystem>

namespace zone
{
    struct Shader
    {
        bool pixel = false;
        uint64_t hash = 0;
        std::vector<uint32_t> code;   // microcode dwords, as the command stream loads them
    };

    // The definitions compiled into the launcher (defs/), read once.
    const Schema& Rules();

    struct BindTables;

    // What became of the zone's vertex shaders (vertex_binding.h).
    struct BindCounts
    {
        uint32_t bound = 0;          // bound for their pass's declaration, as the title binds them
        uint32_t unbound = 0;        // loaded where nothing binds them
        uint32_t noDeclaration = 0;  // in a slot whose declaration type has no stream for a source
        uint32_t ownShader = 0;      // a pass's own vertex shader, past the 15 declaration types
    };

    // Each shader of the zone once, vertex shaders as their binding leaves
    // them. Throws zone::Error when the zone does not walk to its end, or a
    // shader's header cannot be read.
    std::vector<Shader> Shaders(const std::vector<uint8_t>& zone, const BindTables& tables, BindCounts* counts = nullptr);

    // The shaders an executable carries in itself, from its memory image
    // (install::xex::Image, at 0x82000000):
    //   - the SDK's GPU memory move, the image pool's block copy: compiled
    //     shader files whose header -- the same one a shader object holds --
    //     starts with the magic 0x102A11xx; its second word is where the
    //     program starts, and the same table as above says where in the
    //     program the microcode is;
    //   - D3D's own, for its clears and the like: bare microcode the D3D code
    //     copies into the command stream (IM_LOAD_IMMEDIATE) from addresses it
    //     names, read from that code once. The install checks each executable
    //     by its SHA-256, so they are where the table says.
    //   - the memory move's vertex shader again, bound as its draw binds it.
    std::vector<Shader> ExecutableShaders(const std::vector<uint8_t>& image, bool multiplayer, const BindTables& tables);
}
