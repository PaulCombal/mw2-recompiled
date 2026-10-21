// The map's sounds: alias lists from the stock 360 files, and the map's own
// audio encoded as XMA (xma.h) for those no stock file has.
#include "porter.h"
#include "xma.h"

#include <algorithm>
#include <chrono>
#include <cstdio>
#include <functional>

namespace mapport
{
    namespace
    {
        constexpr size_t kEncodedSoundBudgetMb = 12;   // XMA made at most (stock maps carry 4 to 8 MB of sounds)

        // Alias head members the PC's alias keeps (the stock "null" alias's
        // head gives the rest).
        constexpr const char* kHeadFields[] = {
            "sequence", "volMin", "volMax", "pitchMin", "pitchMax", "distMin", "distMax", "velocityMin", "flags",
            "probability", "lfePercentage", "centerPercentage", "startDelay", "envelopMin", "envelopMax",
            "envelopPercentage",
        };

        // The 360 name of a PC sound file (dir/name.wav, .mp3, ...): stock 360
        // sounds have no extension.
        std::string SoundName(const std::string& path)
        {
            const size_t dot = path.rfind('.');
            if (dot == std::string::npos || path.find('/', dot) != std::string::npos) return path;
            const std::string ext = Lower(path.substr(dot));
            return ext == ".wav" || ext == ".mp3" || ext == ".flac" || ext == ".ogg" ? path.substr(0, dot) : path;
        }

        // A PC LoadedSound's audio bytes, or null for one that only names its
        // sound.
        const std::vector<uint8_t>* SoundData(const Struct* ls)
        {
            const Struct* s = Embedded(ls, "sound");
            Node* d = s ? s->Child("data") : nullptr;
            if (Ref* r = As<Ref>(d)) d = r->target;
            const Leaf* lf = As<Leaf>(d);
            return lf && !lf->raw.empty() ? &lf->raw : nullptr;
        }

        // A PC LoadedSound's audio: a .wav file, or plain samples its info
        // describes as 16-bit PCM.
        SoundPcm LoadedPcm(const Struct* ls)
        {
            const Struct* info = Embedded(Embedded(ls, "sound"), "info");
            std::optional<PcmFormat> plain;
            if (info && info->Int("format") == 1 && info->Int("bits") == 16 &&
                (info->Int("channels") == 1 || info->Int("channels") == 2) && info->Int("rate"))
                plain = { uint32_t(info->Int("rate")), int(info->Int("channels")) };
            return ReadSound(*SoundData(ls), plain);
        }

        std::string Megabytes(size_t bytes)
        {
            char s[32];
            std::snprintf(s, sizeof s, "%.1f", double(bytes) / 1048576.0);
            return s;
        }
    }

    // An alias list entry for each sound the map's destructibles need and
    // lack: only its name, which StockSounds fills from the stock file.
    void Porter::AddDestructibleAliases()
    {
        std::set<std::string> have;
        ForEach(root.assets, [&](Node* n) {
            if (Struct* o = As<Struct>(n); o && o->IsAsset("snd_alias_list_t")) have.insert(Lower(AliasName(o).value_or("")));
        });
        const zone::StructInfo* info = P.infos.at("snd_alias_list_t");
        std::vector<Entry*> made;
        std::vector<std::string> names;
        for (const std::string& name : destructibleAliases)
        {
            if (have.count(Lower(name)) || !stockAliases.Has(name)) continue;
            Struct* d = Struct::Zero(info->ctype, false);
            d->asset = info->name;
            d->SetChild("aliasName", A().Make<String>(name));
            made.push_back(A().Make<Entry>("sound", d));
            have.insert(Lower(name));
            names.push_back(name);
        }
        root.assets.insert(root.assets.begin(), made.begin(), made.end());
        if (!made.empty())
            log("  " + std::to_string(made.size()) + " destructible sounds added from the stock 360 files (" + Joined(names) +
                ")");
    }

    // Sound alias lists come from a stock 360 file with the same alias, audio
    // and all, as stock maps carry them: the PC file only names its sound
    // files, which the 360 has nothing under. An alias no stock file has gets
    // the PC's own audio, encoded (the map's loaded sounds, streamed ones the
    // .iwd files have), else the stock "null" alias's silent sound, under its
    // own name. The PC's sound files then go.
    void Porter::StockSounds()
    {
        int swapped = 0;
        std::vector<std::string> silent, missing, played;
        const StockAlias* null = stockAliases.Find("null");
        std::vector<Struct*> ours;
        ForEach(root.assets, [&](Node* n) {
            if (Struct* o = As<Struct>(n); o && o->IsAsset("snd_alias_list_t") && !done.count(o)) ours.push_back(o);
        });
        // Stock aliases share sound files (one alias's points into another's):
        // those from one file are made to stand alone together, so they keep
        // sharing, and go in that file's order, shared parts first.
        std::unordered_map<const Node*, const StockAlias*> take;
        Ordered<const StockFile*, Ordered<const Node*, Node*>> byFile;
        for (Struct* d : ours)
        {
            const std::optional<std::string> n = AliasName(d);
            if (!n || n->starts_with(",")) continue;
            if (const StockAlias* s = stockAliases.Find(*n))
            {
                take[d] = s;
                byFile.FindOrAdd(s->file).FindOrAdd(s->alias, s->alias);
            }
        }
        for (auto& [file, group] : byFile)
        {
            std::vector<Node*> roots;
            for (auto& [k, v] : group) roots.push_back(v);
            Localize(roots);
        }
        std::unordered_map<const StockFile*, size_t> fileOrder;
        for (auto& [k, s] : stockAliases) fileOrder.emplace(s.file, fileOrder.size());
        std::unordered_map<const Node*, std::pair<size_t, int64_t>> rank;
        for (Struct* d : ours)
        {
            auto it = take.find(d);
            if (it == take.end()) continue;
            Struct* made = it->second->alias->Copy();
            made->slot = nullptr;
            Replace(d, *made);
            rank[d] = { fileOrder.at(it->second->file), it->second->order };
            swapped++;
        }
        std::vector<size_t> at;
        std::vector<Entry*> ranked;
        for (size_t i = 0; i < root.assets.size(); i++)
            if (As<Struct>(root.assets[i]->asset) && rank.count(root.assets[i]->asset))
            {
                at.push_back(i);
                ranked.push_back(root.assets[i]);
            }
        std::stable_sort(ranked.begin(), ranked.end(), [&](Entry* a, Entry* b) { return rank[a->asset] < rank[b->asset]; });
        for (size_t k = 0; k < at.size(); k++) root.assets[at[k]] = ranked[k];
        // Silent ones share the first one's sound (in the order they are written).
        std::unordered_map<const Node*, size_t> pos;
        for (size_t i = 0; i < root.assets.size(); i++) pos.emplace(root.assets[i]->asset, i);
        auto where = [&](const Struct* d) {
            auto it = pos.find(d);
            return it == pos.end() ? root.assets.size() : it->second;
        };
        std::stable_sort(ours.begin(), ours.end(), [&](Struct* a, Struct* b) { return where(a) < where(b); });
        for (Entry* e : root.assets)
            if (Struct* ls = As<Struct>(e->asset); ls && e->type == "loaded_sound")
                if (const std::optional<std::string> n = PoolName(ls); Named(n) && !OnlyNamed(n)) pcLoaded.emplace(Lower(*n), ls);
        Struct* nullFile = nullptr;
        for (Struct* d : ours)
        {
            if (rank.count(d)) continue;
            const std::optional<std::string> name = AliasName(d);
            if (!name || name->empty() || name->starts_with(",")) continue;
            if (!null)
            {
                missing.push_back(*name);
                continue;
            }
            Struct* made = DeepCopy(CopyIn(null->alias));
            // (An ambience track the script no longer plays isn't encoded.)
            const std::vector<HeadAudio> audio = ambientSwaps.Has(*name) ? std::vector<HeadAudio>{} : PcAudio(d);
            if (!audio.empty()) EncodedHeads(made, audio);
            NameLoadedCurves(made);
            made->SetChild("aliasName", A().Make<String>(*name));
            Node* head = made->Child("head");
            if (Ref* r = As<Ref>(head)) head = r->target;
            EachHeld(head, [&](Node* x) {
                Struct* h = As<Struct>(x);
                if (!h) return;
                h->SetChild("aliasName", A().Make<String>(*name));
                h->SetPtr("aliasName", kFollowing);
                Struct* sf = As<Struct>(h->Child("soundFile"));
                if (!sf || !audio.empty()) return;
                if (!nullFile)
                {
                    nullFile = sf;
                    return;
                }
                // Each keeps its own sound file record and shares the first one's
                // silent sound through that sound's slot. Pointing at the first
                // one's record pointed into another asset's temporary data, which
                // the 360 reuses once that asset is loaded.
                Struct* firstU = Embedded(nullFile, "u");
                Struct* first = firstU ? As<Struct>(firstU->Child("loadSnd")) : nullptr;
                Struct* u = Embedded(sf, "u");
                if (first && first->slot && u && As<Struct>(u->Child("loadSnd")))
                {
                    Ref* r = A().Make<Ref>(1);
                    r->target = first->slot;
                    u->SetChild("loadSnd", r);
                }
            });
            Replace(d, *made);
            (audio.empty() ? silent : played).push_back(*name);
        }
        // The PC's sound files (each only a name) are pointed at by nothing now.
        std::unordered_set<const Node*> held;
        for (Entry* e : root.assets)
        {
            if (e->type == "loaded_sound") continue;
            ForEach(e->asset, [&](Node* n) {
                Struct* o = As<Struct>(n);
                if (!o || !o->Ch()) return;
                for (auto& [k, c] : o->Ch()->items)
                    EachHeld(c, [&](Node* x) {
                        if (Ref* r = As<Ref>(x))
                        {
                            Node* t = r->target;
                            if (Slot* s = As<Slot>(t)) t = s->asset;
                            held.insert(t);
                        }
                        else if (As<Struct>(x))
                            held.insert(x);
                    });
            });
        }
        const size_t before = root.assets.size();
        std::erase_if(root.assets, [&](Entry* e) {
            return e->type == "loaded_sound" && As<Struct>(e->asset) && !held.count(e->asset);
        });
        log("  sounds: " + std::to_string(swapped) + " aliases from the stock 360 files, " + std::to_string(silent.size()) +
            " silent (no stock copy), " + std::to_string(before - root.assets.size()) + " PC sound files left out");
        if (!played.empty())
        {
            size_t sounds = 0, bytes = 0, resident = 0;
            for (const auto& [key, sound] : encoded)
            {
                if (sound.xma)
                {
                    sounds++;
                    bytes += sound.xma->data.size();
                }
                resident += sound.resident;
            }
            log("  " + std::to_string(played.size()) + " aliases play the PC's audio, encoded for the 360 (" +
                std::to_string(sounds) + " sounds, " + Megabytes(bytes) + " MB; " + std::to_string(resident) +
                " more the game has loaded)");
        }
        if (encodedBytes >= kEncodedSoundBudgetMb * 1024 * 1024)
            Warn("encoded sounds reached " + std::to_string(kEncodedSoundBudgetMb) + " MB: the rest stay silent");
        if (compressedSounds) Warn("some sounds are .mp3 or compressed .wav files, which aren't decoded: they stay silent");
        if (!noAudio.empty())
            log("    no audio found for " + std::to_string(noAudio.size()) + " sound files" +
                (gameIwds.empty() ? " (give the PC game folder for its streamed sounds)" : ""));
        if (!silent.empty()) log("    silent: " + Joined(silent, 20, " ..."));
        if (!missing.empty())
            Warn(std::to_string(missing.size()) +
                 " sound aliases have no stock copy and no stock \"null\" alias was found to stand in: add a stock map, "
                 "they stay as the PC has them");
    }

    // Curves (SndCurve) in an alias list made here that the game has loaded
    // only name them, as stock maps do: a copy of its own in each took one of
    // the game's 64 curve places apiece.
    void Porter::NameLoadedCurves(Struct* alias)
    {
        for (Node* n : Collect(alias))
        {
            Struct* o = As<Struct>(n);
            if (!o || !o->Ch()) continue;
            auto swap = [&](Node*& x) {
                Struct* c = As<Struct>(x);
                if (!c || !c->IsAsset("SndCurve")) return;
                const std::string name = PoolName(c).value_or("");
                if (name.empty() || name.starts_with(",") || !loaded.count({ "SndCurve", Lower(name) })) return;
                Struct* ref = Reference("SndCurve", name);
                done.insert(ref);
                x = ref;
            };
            for (auto& [k, c] : o->Ch()->items)
            {
                if (Array* a = As<Array>(c))
                    for (Node*& x : a->items) swap(x);
                else if (PtrList* pl = As<PtrList>(c))
                    for (Node*& x : pl->items) swap(x);
                else swap(c);
            }
        }
    }

    const ZipIndex& Porter::SoundFiles()
    {
        if (!soundFiles)
        {
            soundFiles.emplace();
            for (const std::vector<Zip>& zips : { std::cref(gameIwds), std::cref(iwds) })
                for (const Zip& z : zips)
                    for (size_t k = 0; k < z.Names().size(); k++)
                    {
                        std::string low = Lower(z.Names()[k]);
                        std::replace(low.begin(), low.end(), '\\', '/');
                        if (low.starts_with("sound/")) (*soundFiles)[low.substr(6)] = { &z, k };
                    }
        }
        return *soundFiles;
    }

    std::vector<Porter::HeadAudio> Porter::PcAudio(Struct* alias)
    {
        Node* heads = alias->Child("head");
        if (Ref* r = As<Ref>(heads)) heads = r->target;
        std::vector<HeadAudio> out;
        EachHeld(heads, [&](Node* x) {
            Struct* h = As<Struct>(x);
            Struct* file = h ? As<Struct>(h->Child("soundFile")) : nullptr;
            if (!file) return;
            Struct* u = Embedded(file, "u");
            std::string key;
            std::function<SoundPcm()> read;
            if (file->Int("type") == 1)   // loaded
            {
                Node* n = u ? u->Child("loadSnd") : nullptr;
                if (Ref* r = As<Ref>(n)) n = As<Slot>(r->target) ? As<Slot>(r->target)->asset : r->target;
                if (Struct* ls = As<Struct>(n))
                {
                    key = PoolName(ls).value_or("");
                    key = Lower(WithoutCommas(std::move(key)));
                    // (An alias of the map's may only name a sound the map loads.)
                    if (!SoundData(ls))
                        if (auto it = pcLoaded.find(key); it != pcLoaded.end()) ls = it->second;
                    if (SoundData(ls)) read = [ls] { return LoadedPcm(ls); };
                }
            }
            else if (file->Int("type") == 2)   // streamed: from the .iwd files
            {
                Struct* ss = Embedded(u, "streamSnd");
                const String* dir = ss ? As<String>(ss->Child("dir")) : nullptr;
                const String* name = ss ? As<String>(ss->Child("name")) : nullptr;
                if (name)
                {
                    key = (dir && !dir->b.empty() ? dir->b + "/" : "") + name->b;
                    std::replace(key.begin(), key.end(), '\\', '/');
                    key = Lower(key);
                    if (auto it = SoundFiles().find(key); it != SoundFiles().end())
                        read = [found = it->second] { return ReadSound(found.zip->Read(found.index)); };
                }
            }
            if (key.empty()) return;
            if (!encoded.count(key))
            {
                EncodedSound sound;
                // The game has it loaded already (common_mp): named only, as stock maps do.
                if (loaded.count({ "LoadedSound", SoundName(key) })) sound.resident = true;
                else
                {
                    const SoundPcm pcm = read ? read() : SoundPcm{};
                    compressedSounds |= pcm.compressed;
                    if (pcm.pcm && encodedBytes < kEncodedSoundBudgetMb * 1024 * 1024)
                    {
                        sound.xma = Encode(key, *pcm.pcm);
                        encodedBytes += sound.xma->data.size();
                    }
                    else if (!pcm.pcm)
                        noAudio.insert(key);
                }
                encoded.emplace(key, std::move(sound));
            }
            if (encoded.at(key).Plays()) out.push_back({ h, key });
        });
        return out;
    }

    // One sound encoded; the log shows a long one going (ambience tracks take
    // a while).
    Xma Porter::Encode(const std::string& name, const Pcm& pcm)
    {
        const size_t seconds = pcm.samples.size() / (size_t(pcm.channels) * pcm.rate);
        if (seconds >= 30) log("    encoding " + name + " (" + std::to_string(seconds) + " seconds of sound)");
        const auto start = std::chrono::steady_clock::now();
        Xma x = EncodeXma(pcm.samples, pcm.rate, pcm.channels);
        encodeSeconds += std::chrono::duration<double>(std::chrono::steady_clock::now() - start).count();
        if (++encodeCount % 20 == 0)
            log("    " + std::to_string(encodeCount) + " sounds encoded so far (" + Megabytes(encodedBytes + x.data.size()) +
                " MB, " + std::to_string(int(encodeSeconds)) + " s)");
        return x;
    }

    // Alias list `alias` (a copy of the stock "null" one) gets a head for each
    // (PC head, sound) in `audio`: the null head with the PC's numbers, playing
    // its own 360 sound (a loaded one, as stock aliases' are). A sound used
    // again points at its first copy.
    void Porter::EncodedHeads(Struct* alias, const std::vector<HeadAudio>& audio)
    {
        Node* held = alias->Child("head");
        if (Ref* r = As<Ref>(held)) held = r->target;
        Array* list = As<Array>(held);
        Struct* tpl = As<Struct>(list ? list->items.at(0) : held);
        std::vector<Node*> heads;
        for (const auto& [pc, key] : audio)
        {
            Struct* h = DeepCopy(tpl);
            for (const char* k : kHeadFields)
                if (pc->Has(k) && h->Has(k))
                {
                    if (h->M(k).type->isFloat) h->SetF(k, pc->F(k));
                    else h->Set(k, pc->Int(k));
                }
            // Bit 7: a loaded sound, bit 8: streamed (stock: 1,684 and 3,802 of 5,486 heads).
            h->Set("flags", (h->Int("flags") | 0x80) & ~0x100);
            for (const char* k : { "secondaryAliasName", "chainAliasName" })
                if (const String* name = As<String>(pc->Child(k)); name && h->Has(k))
                {
                    h->SetPtr(k, kFollowing);
                    h->SetChild(k, A().Make<String>(name->b));
                }
            Struct* u = Embedded(As<Struct>(h->Child("soundFile")), "u");
            EncodedSound& sound = encoded.at(key);
            if (sound.resident)
            {
                Struct* ref = Reference("LoadedSound", SoundName(key));
                done.insert(ref);
                u->SetChild("loadSnd", ref);
            }
            else if (sound.first)
            {
                Ref* r = A().Make<Ref>(1);
                r->target = sound.first->slot;
                u->SetChild("loadSnd", r);
            }
            else
            {
                Struct* ls = As<Struct>(u->Child("loadSnd"));
                sound.first = ls;
                if (!ls->slot) ls->slot = A().Make<Slot>(ls);
                ls->slot->asset = ls;
                ls->SetChild("name", A().Make<String>(SoundName(key)));
                const Xma& x = *sound.xma;
                Struct* s = Embedded(ls, "sound");
                Struct* info = Embedded(s, "info");
                const Leaf* data = As<Leaf>(info->Child("data"));
                info->SetChild("data", A().Make<Leaf>(data->t, int64_t(x.data.size()), x.data, data->big));
                info->Set("dataSize", int64_t(x.data.size()));
                // Where the audio starts and ends (in bits), and which quarter of
                // its last frame it ends in (3: the whole frame plays).
                const uint32_t bounds[] = { 0xFFFFFFFF, 32, uint32_t(x.endBit), (3u << 24) | (3u << 16) };
                for (size_t k = 0; k < info->M("unknown").Elements(); k++) info->Set("unknown", k < 4 ? bounds[k] : 0, k);
                // XMA, one stream, its rate, one channel, its length in ms.
                const size_t n = s->M("unknown").Elements();
                for (size_t k = 0; k < n; k++) s->Set("unknown", 0, k);
                s->Set("unknown", 0x04000000, 0);
                s->Set("unknown", 0x01000000, 1);
                s->Set("unknown", x.rate, 2);
                s->Set("unknown", 0x01000000, 3);
                s->Set("unknown", x.Ms(), n - 1);
                Struct* table = Embedded(s, "seekTable");
                const Leaf* old = As<Leaf>(table->Child("data"));
                std::vector<uint32_t> words{ 1, uint32_t(x.seek.size()) };
                words.insert(words.end(), x.seek.begin(), x.seek.end());
                std::vector<uint8_t> bytes(words.size() * 4);
                for (size_t k = 0; k < words.size(); k++) Store(&bytes[k * 4], 4, words[k], old->big);
                table->Set("size", int64_t(words.size()));
                table->SetChild("data", A().Make<Leaf>(old->t, int64_t(words.size()), std::move(bytes), old->big));
            }
            heads.push_back(h);
        }
        if (list) list->items = std::move(heads);
        else
        {
            Array* made = A().Make<Array>();
            made->items = std::move(heads);
            alias->SetChild("head", made);
        }
        alias->Set("count", int64_t(audio.size()));
    }
}
