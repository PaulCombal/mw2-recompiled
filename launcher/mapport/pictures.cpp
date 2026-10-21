// Pictures: the PC keeps a map's pictures in its .iwd (and lightmaps and
// reflection probes in the fastfile); the 360 map carries them in its own
// file, tiled for the console's GPU, each with the header of a stock picture
// of its format.
#include "porter.h"
#include "paths.h"

#include <algorithm>

namespace mapport
{
    namespace
    {
        // A 16x16 DXT1 picture of one block repeated.
        std::vector<Mip> Plain(uint16_t colour, uint32_t indexes)
        {
            Mip block(8);
            Store(block.data(), 2, colour, false);
            Store(block.data() + 2, 2, colour, false);
            Store(block.data() + 4, 4, indexes, false);
            Mip mip;
            for (int k = 0; k < 16; k++) mip.insert(mip.end(), block.begin(), block.end());
            return { mip };
        }
    }

    bool Porter::InIwd(const std::string& name) const { return mapPictures.count(Lower(name)) != 0; }

    // A picture a stock map streams that the map doesn't bring in its own .iwd
    // (the PC game's pictures are the 360's; a map's own may not be).
    bool Porter::StreamsFromStock(const std::string& name) const
    {
        const std::string low = Lower(name);
        return stockStreamed.Has(low) && !mapPictures.count(low);
    }

    // ---- making 360 pictures ------------------------------------------------

    // Image `d` (in place) as a 360 picture with these linear mips (for a cube
    // map, each face's chain): the header of `tpl`, or of a stock picture of
    // the format, with the size and levels changed.
    void Porter::BuildImage(Struct* d, Gpu f, int width, int height, const std::vector<std::vector<Mip>>& faces, bool cube,
                            Struct* tpl)
    {
        const int gpu = int(f);
        auto stock = [&](int g, bool c) -> Struct* {
            auto it = templates.find({ g, c });
            return it == templates.end() ? nullptr : it->second;
        };
        if (!tpl) tpl = stock(gpu, cube);
        bool makeCube = false;
        if (!tpl && f == Gpu::DXN)
            if (Struct* dxt5 = stock(int(Gpu::DXT5), cube))
            {
                // Stock DXN pictures (normal maps) all stream from the disc, so no
                // file carries one to copy: a DXT5 one with the format changed.
                tpl = dxt5->Copy();
                uint8_t* texture = tpl->Bytes("texture").data();
                Store(texture + 32, 4, (Load(texture + 32, 4, false) & ~0x3Fu) | 0x31, false);
                tpl->Set("textureFormat", (tpl->Int("textureFormat") & ~int64_t(0x3F)) | 0x31);
                templates[{ int(Gpu::DXN), cube }] = tpl;
            }
        if (!tpl && cube)
        {
            // No stock cube map of this format in a file: a flat one made a cube.
            tpl = stock(gpu, false);
            makeCube = true;
        }
        if (!tpl)
            throw PortError(std::string("no stock 360 ") + FormatOf(f).name + (cube ? " cube" : "") +
                            " picture to copy settings from");
        std::vector<uint8_t> pixels;
        int levels;
        if (cube)
        {
            // Six faces, each tiled on its own: every face's top level, then each
            // further mip slice for all six (as stock reflection probes are).
            levels = int(faces[0].size());
            for (const auto& c : faces) levels = std::min(levels, int(c.size()));
            if (levels == 1)
                for (const auto& c : faces)
                {
                    const std::vector<uint8_t> t = Tile({ c[0] }, width, height, f, true);
                    pixels.insert(pixels.end(), t.begin(), t.end());
                }
            else
            {
                const auto [plan, total] = Plan(width, height, f);
                std::set<size_t> starts;
                for (int i = 0; i < levels && i < int(plan.size()); i++) starts.insert(plan[size_t(i)].base);
                std::vector<size_t> cuts(starts.begin(), starts.end());
                cuts.push_back(total);
                std::vector<std::vector<uint8_t>> tiled;
                for (const auto& c : faces) tiled.push_back(Tile({ c.begin(), c.begin() + levels }, width, height, f));
                for (size_t i = 0; i + 1 < cuts.size(); i++)
                    for (const auto& t : tiled)
                    {
                        const size_t a = std::min(cuts[i], t.size()), b = std::min(cuts[i + 1], t.size());
                        pixels.insert(pixels.end(), t.begin() + ptrdiff_t(a), t.begin() + ptrdiff_t(std::max(a, b)));
                    }
            }
        }
        else
        {
            levels = int(faces[0].size());
            pixels = Tile(faces[0], width, height, f, levels == 1);
        }
        if (f == Gpu::L8) pixels = Swap(pixels, f);   // 8-bit data isn't byte-swapped on the 360
        std::vector<uint8_t> header(tpl->Bytes("texture").begin(), tpl->Bytes("texture").end());
        uint32_t dw[6];
        for (int k = 0; k < 6; k++) dw[k] = uint32_t(Load(header.data() + 28 + 4 * k, 4, false));
        const uint32_t pitch = uint32_t(AlignUp(size_t(width), FormatOf(f).blockWidth == 4 ? 128 : 32) / 32);
        dw[0] = (dw[0] & ~(0x1FFu << 22)) | (pitch << 22);
        dw[2] = (dw[2] & ~0x3FFFFFFu) | uint32_t(width - 1) | (uint32_t(height - 1) << 13);
        dw[4] = (dw[4] & ~(0xFu << 6)) | (uint32_t(levels - 1) << 6);
        const uint32_t mipAddress = levels > 1 ? uint32_t((Layout(width, height, 0, f).slice * (cube ? 6 : 1)) >> 12) : 0;
        dw[5] = (dw[5] & 0xFFFu & ~(1u << 11)) | (mipAddress << 12) | (levels > 1 ? 1u << 11 : 0);
        if (makeCube)
        {
            dw[2] = (dw[2] & 0x3FFFFFFu) | (5u << 26);   // six faces
            dw[5] = (dw[5] & ~(3u << 9)) | (3u << 9);    // dimension: cube
        }
        for (int k = 0; k < 6; k++) Store(header.data() + 28 + 4 * k, 4, dw[k], false);

        Struct* made = tpl->Copy();
        std::copy(header.begin(), header.end(), made->Bytes("texture").begin());
        made->Set("cardMemory", int64_t(pixels.size()));
        made->Set("width", width);
        made->Set("height", height);
        made->Set("depth", 1);
        made->Set("levelCount", levels);
        made->Set("streaming", 0);
        made->SetPtr("pixels", kFollowing);
        made->SetPtr("name", kFollowing);
        const auto streams = made->Bytes("streams");
        std::fill(streams.begin(), streams.end(), uint8_t(0));
        for (const char* k : { "mapType", "semantic", "category" })
            if (d->Has(k)) made->Set(k, d->Int(k));
        if (cube) made->Set("mapType", 5);
        Children* ch = A().MakeChildren();
        ch->Set("name", d->Child("name"));
        ch->Set("pixels", A().Make<Leaf>(X.defs.types.at("unsigned char"), int64_t(pixels.size()), std::move(pixels), true));
        made->SetCh(ch);
        made->asset = X.infos.at("GfxImage")->name;
        Slot* slot = d->slot;
        const bool forward = d->forward;
        d->Assign(*made);
        if (slot) d->slot = slot;
        if (forward) d->forward = true;
    }

    // A small plain magenta picture (what's missing is easy to see).
    Struct* Porter::Magenta(Struct* d, std::optional<std::string> name)
    {
        if (name) d->SetChild("name", A().Make<String>(*name));
        BuildImage(d, Gpu::DXT1, 16, 16, { Plain(0xF81F, 0) }, false);
        d->Set("category", 3);
        done.insert(d);
        return d;
    }

    // A small picture alpha tested away entirely.
    Struct* Porter::SeeThrough(Struct* d, const std::string& name)
    {
        d->SetChild("name", A().Make<String>(name));
        BuildImage(d, Gpu::DXT1, 16, 16, { Plain(0, 0xFFFFFFFF) }, false);
        d->Set("category", 3);
        done.insert(d);
        return d;
    }

    // The built-in picture standing in for one the map doesn't bring: flat for
    // a normal map, black (no shine) for a specular one; for the rest nothing
    // (magenta stands in).
    std::optional<std::string> Porter::StandIn(const Struct* d, const std::string& name)
    {
        const int64_t semantic = d->Int("semantic");
        if (semantic == 5 || name.ends_with("_nml") || name.ends_with("_n")) return "$identitynormalmap";
        if (semantic == 8 || name.ends_with("_spc") || name.ends_with("_s")) return "$black";
        return std::nullopt;
    }

    // A copy of a stock water picture under `name` (the game draws the waves
    // into it; the PC file has no pixels for it), or null.
    Struct* Porter::WaterImage(Struct* d, const std::string& name)
    {
        Struct* best = nullptr;
        for (auto& [k, v] : library)
        {
            if (k.type != "GfxImage" || v->Int("category") != 5 || v->Ptr("pixels") != kFollowing) continue;
            if (!best || (v->Int("width") == d->Int("width") && v->Int("height") == d->Int("height"))) best = v;
        }
        if (!best) return nullptr;
        Struct* made = CopyIn(best);
        Children* ch = A().MakeChildren();
        if (made->Ch()) *ch = *made->Ch();
        made->SetCh(ch);
        ch->Set("name", A().Make<String>(name));
        if (Leaf* lf = As<Leaf>(ch->Get("pixels"))) ch->Set("pixels", A().Make<Leaf>(lf->t, lf->n, lf->raw, lf->big));
        return made;
    }

    // Stock 360 probes carry the same pattern in alpha in every probe of a map:
    // ours get a stock one's (the same size, so the same place in the tiling).
    void Porter::ProbeAlpha(Struct* d)
    {
        Leaf* px = As<Leaf>(d->Child("pixels"));
        if (!px) return;
        for (auto& [k, v] : library)
        {
            if (k.type != "GfxImage" || !k.name.starts_with("*reflection_probe")) continue;
            const Leaf* theirs = As<Leaf>(v->Child("pixels"));
            if (!theirs || theirs->n != px->n || v->Int("width") != d->Int("width")) continue;
            for (size_t i = 0; i < px->raw.size() && i < theirs->raw.size(); i += 4) px->raw[i] = theirs->raw[i];
            return;
        }
    }

    // ---- the map's pictures -------------------------------------------------

    bool Porter::PreGfxImage(Struct* d)
    {
        std::string name = AssetName(d).value_or(Name(d).value_or(""));   // (Name: one shared with another asset)
        if (d->Int("category") == 5 && !name.starts_with(","))
        {
            // Water: a stand-in of another size would have the game write the
            // waves past its end.
            Struct* made = WaterImage(d, name);
            if (!made)
                throw PortError("water picture " + name +
                                ": no stock 360 water picture in the stock files given to copy (mp_favela has some)");
            Replace(d, *made);
            return true;
        }
        if (name.starts_with(","))
        {
            // The PC file only names it: so does the 360 file when the 360 has it
            // loaded; else it becomes a picture of the map's own.
            name.erase(0, 1);
            if (resident.Has({ "GfxImage", name }) || named.count({ "GfxImage", name }))
            {
                Replace(d, *Reference("GfxImage", name));
                return true;
            }
            d->SetChild("name", A().Make<String>(name));
        }
        // Built-in pictures ($identitynormalmap, ...) stay the game's own even
        // when an .iwd carries a copy: a second one would replace the resident one.
        if (resident.Has({ "GfxImage", name }) && (name.starts_with("$") || !InIwd(name)))
        {
            Replace(d, *Reference("GfxImage", name));
            return true;
        }
        if (StreamsFromStock(name))
        {
            streamedStock++;
            Replace(d, *StockCopy(DeepCopy(*stockStreamed.Find(Lower(name)))));
            return true;
        }
        Struct* texture = Embedded(d, "texture");
        Struct* loadDef = texture ? As<Struct>(texture->Child("loadDef")) : nullptr;
        if (loadDef && loadDef->Int("resourceSize")) ImageFromLoadDef(d, loadDef);
        else if (!PictureFromFiles(d, name))
        {
            // Not in the map's .iwd, or no copy there can be read: a built-in
            // picture stands in, so the map still loads.
            const std::optional<std::string> stand = StandIn(d, name);
            missingImages.push_back({ name, stand });
            if (!stand) Magenta(d);
            else
            {
                Replace(d, *Reference("GfxImage", *stand));
                return true;
            }
        }
        done.insert(d);
        return true;
    }

    // Fills image `d` from the map's .iwd, else the PC game's; false when
    // neither has a copy that can be read (an unreadable one is noted, not
    // fatal).
    bool Porter::PictureFromFiles(Struct* d, const std::string& name)
    {
        const std::string low = Lower(name);
        for (const auto* index : { &mapPictures, &gamePictures })
        {
            auto found = index->find(low);
            if (found == index->end()) continue;
            const auto [zip, entry] = found->second;
            try
            {
                ImageFromIwd(d, name, zip->Read(entry));
            }
            catch (const Error& e)
            {
                unreadable.push_back(name + " (" + Utf8(zip->Path().filename()) + ": " + e.what() + ")");
                continue;
            }
            if (index == &gamePictures) fromGame++;
            return true;
        }
        return false;
    }

    void Porter::ImageFromIwd(Struct* d, const std::string& name, const std::vector<uint8_t>& data)
    {
        Picture pic = ReadIwi(data);
        const bool loadscreen = name.starts_with("loadscreen");
        const int limit = loadscreen ? 1024 : 2048;
        int w = pic.width, h = pic.height;
        std::vector<Mip> mips = std::move(pic.mips);
        if (!pic.cube)
        {
            if (PictureSize(w, h, limit) != std::pair{ w, h })
            {
                Fitted fit = FitPicture(pic.format, w, h, mips, limit);
                w = fit.width;
                h = fit.height;
                mips = std::move(fit.mips);
            }
            auto drop = mipDrop.find(name);
            if (const int k = drop == mipDrop.end() ? 0 : drop->second; k && int(mips.size()) > k)
            {
                mips.erase(mips.begin(), mips.begin() + k);
                w = std::max(1, w >> k);
                h = std::max(1, h >> k);
            }
        }
        // Stock loading screens have no mipmaps; with them the console stopped
        // loading a big map ("MT_GetSize: max allocation exceeded").
        if (loadscreen) mips.resize(1);
        Gpu f = pic.format;
        // (semantic 5, or named _nml: mp_afghan's wavy_nml, semantic 3, is DXN in stock too)
        if (f == Gpu::DXT5 && !pic.cube && (d->Int("semantic") == 5 || name.ends_with("_nml")))
        {
            mips = NormalMapsToDxn(mips);
            f = Gpu::DXN;
            dxnCount++;
        }
        std::vector<std::vector<Mip>> faces;
        if (pic.cube)
            for (Mip& m : mips) faces.push_back({ std::move(m) });
        else faces.push_back(std::move(mips));
        BuildImage(d, f, w, h, faces, pic.cube);
        // Pictures from files are "load from file" on the 360; stock loading
        // screens are plain 2D pictures.
        d->Set("category", 3);
        if (loadscreen) d->Set("semantic", 0);
    }

    // Pictures the PC keeps in the fastfile: lightmaps, reflection probes, $outdoor.
    void Porter::ImageFromLoadDef(Struct* d, Struct* loadDef)
    {
        const std::string name = AssetName(d).value_or("");
        const int64_t format = loadDef->Int("format");
        const Leaf* leaf = As<Leaf>(loadDef->Late("data"));
        const std::vector<uint8_t> data = leaf ? leaf->raw : std::vector<uint8_t>{};
        const int w = int(d->Int("width")), h = int(d->Int("height"));
        const bool cube = (loadDef->Int("flags") & 4) || d->Int("mapType") == 5;
        size_t bpp;
        Gpu out;
        if (format == 21)   // A8R8G8B8: a DDS's byte order, which tiling takes
        {
            bpp = 4;
            out = Gpu::ARGB8;
        }
        else if (format == 50)
        {
            // L8. The 360 keeps the primary lightmap as DXT3A (4 bits a pixel, as
            // stock maps do); other single-channel pictures as plain 8-bit.
            bpp = 1;
            out = name.starts_with("*lightmap") ? Gpu::DXT3A : Gpu::L8;
        }
        else throw PortError("picture " + name + ": PC format " + std::to_string(format) + " isn't supported yet");
        if (w < 1 || h < 1 || w > 8192 || h > 8192)
            throw PortError("picture " + name + " is " + std::to_string(w) + "x" + std::to_string(h));
        const size_t count = cube ? 6 : 1, face = data.size() / count, top = size_t(w) * size_t(h) * bpp;
        auto slice = [&](size_t from, size_t n) {
            from = std::min(from, data.size());
            return Mip(data.begin() + ptrdiff_t(from), data.begin() + ptrdiff_t(std::min(data.size(), from + n)));
        };
        std::vector<std::vector<Mip>> faces;
        for (size_t i = 0; i < count; i++) faces.push_back({ slice(i * face, top) });
        if (cube && out == Gpu::ARGB8)
        {
            // Reflection probes keep every face's mips: the smaller ones are
            // blurrier copies for less glossy surfaces (with the top one alone
            // every shiny surface reflects the sharp picture).
            for (size_t i = 0; i < count; i++)
            {
                std::vector<Mip> chain;
                size_t at = i * face;
                int mw = w, mh = h;
                while (at + size_t(mw) * size_t(mh) * bpp <= (i + 1) * face)
                {
                    chain.push_back(slice(at, size_t(mw) * size_t(mh) * bpp));
                    at += size_t(mw) * size_t(mh) * bpp;
                    if (mw == 1 && mh == 1) break;
                    mw = std::max(1, mw / 2);
                    mh = std::max(1, mh / 2);
                }
                if (name.starts_with("*reflection_probe"))
                    for (Mip& m : chain) m = DecodeProbe(m);
                faces[i] = std::move(chain);
            }
        }
        if (out == Gpu::DXT3A)
            for (auto& c : faces)
                for (Mip& m : c) m = EncodeDxt3a(m, w, h);
        Struct* tpl = nullptr;
        if (name.starts_with("*lightmap"))
            if (Struct** t = lightmapTemplates.Find(name.substr(name.rfind('_') + 1))) tpl = *t;
        if (!cube) faces.resize(1);
        BuildImage(d, out, w, h, faces, cube, tpl);
        if (name.starts_with("*reflection_probe")) ProbeAlpha(d);
    }
}
