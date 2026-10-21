#pragma once
// The conversion of one PC fastfile's tree into the 360's (port.h runs it).
//
// Every struct is carried over to the 360's struct of the same name member by
// member (numbers in big-endian, members matched by name); what the two
// platforms keep differently is converted by hooks on the structs concerned:
// map and model vertices, placed models, the world's surfaces and culling
// trees, materials and their render state, pictures. What the 360 can't run
// from the PC file comes from the stock 360 files given: shader sets, the
// render state of materials using them, effects, sounds, animations, models
// and the teams' assets.
#include "images.h"
#include "ordered.h"
#include "query.h"
#include "reader.h"
#include "xma.h"
#include "zip.h"
#include "../zone/rules.h"

#include <array>
#include <filesystem>
#include <functional>
#include <memory>
#include <map>
#include <set>
#include <unordered_map>
#include <unordered_set>

namespace mapport
{
    struct PortError : Error
    {
        using Error::Error;
    };

    using Log = std::function<void(const std::string&)>;

    // Entries of zip archives (.iwd files) by name in lower case.
    struct ZipEntry
    {
        const Zip* zip;
        size_t index;
    };
    using ZipIndex = std::unordered_map<std::string, ZipEntry>;

    // A stock 360 file read for the conversion.
    struct StockFile
    {
        std::filesystem::path path;
        std::string base;          // its name without .ff, lower case
        std::unique_ptr<Arena> arena = std::make_unique<Arena>();   // its tree's nodes
        Tree tree;
        bool resident = false;     // the game keeps it loaded (code_post_gfx_mp, common_mp)
    };

    // A match's two sides, by their teams' names (as arena files give them).
    struct Teams
    {
        std::string allies, axis;
    };

    // A part in another colour: the stock model's colour, and the map's.
    struct ColourSwap
    {
        std::string stock, map;
    };

    // A breakable part to copy in: the name needed, the stock model, its
    // file, and the colour swap when the stock part has another colour.
    struct Breakable
    {
        std::string need, src;
        std::filesystem::path path;
        std::optional<ColourSwap> swap;
    };

    class Porter
    {
    public:
        // `iwds`: the map's own .iwd files; `gameIwds`: the PC game's, in the
        // order it loads them, for pictures and sounds the map's lack (they
        // are only read from, and must outlive the Porter).
        Porter(Tree& pc, std::vector<StockFile*> refs, std::vector<Zip> iwds, const std::vector<Zip>& gameIwds, Log log,
               int textureBudgetMb);

        // Turns the tree into the 360's.
        void Convert();
        // Copies in what the map's two teams need and sets them in its script;
        // returns the teams used.
        Teams AddTeams(const Teams& wanted);

        // A picture a stock map streams from the console's picture packs, by
        // its name in lower case (the first one given stays).
        void KnowStreamed(const std::string& name, Struct* image) { stockStreamed.FindOrAdd(name, image); }

        std::vector<Breakable> breakables;
        std::vector<std::string> destructibleAliases;
        std::string mapName;
        std::vector<std::string> warnings;
        void Warn(const std::string& msg);

        const zone::Schema& P;
        const zone::Schema& X;

    private:
        // ---- converting structs (convert.cpp) ------------------------------
        void ConvertStruct(Struct* d, const zone::Type* tp, const zone::Type* tx, bool track = true);
        void ConvertInto(Struct* d, const zone::Type* tp, Struct* x, bool member, bool track);
        void ConvertMember(Struct* d, const zone::Member& mp, Struct* x, const zone::Member& mx, bool track);
        void ConvertUnion(Struct* d, const zone::Type* tp, Struct* x, bool track);
        void ConvertChildren(Struct* d, const zone::Type* tp, const zone::Type* tx);
        Node* ConvertChild(Node* c, const zone::Type* tp, const zone::Type* tx);
        Node* ConvertLeaf(Leaf* lf, const zone::Type* tx);
        void ConvertAsset(Struct* d);
        // Hooks on a struct before (it may handle the struct itself, returning
        // true) and after its conversion.
        bool Pre(Struct* d, const zone::Type* tp, const zone::Type* tx, Struct* x);
        void Post(Struct* d, const zone::Type* tx);

        // ---- the conversion's steps (porter.cpp) ---------------------------
        void PlanTextureBudget();
        void EditScript(Struct* d);
        std::string AmbientTrack(const std::string& name);
        std::vector<Struct*> MarkOrphans(const std::unordered_set<const Node*>& gone, bool newOnly);
        void OwnReferenceCopies();
        void PicturesBeforeComposites(size_t start, size_t end);
        void RenameClashes();
        void ShareEffectPictures(const std::unordered_set<const Node*>& ours);
        void NameCommonAssets();
        void EffectSisterMaterials();

        // ---- stock assets (stock.cpp) --------------------------------------
        Struct* CopyIn(Struct* src);
        void Localize(const std::vector<Node*>& roots);
        Struct* StockCopy(Struct* src);
        void Replace(Struct* d, const Struct& made);
        Struct* Reference(const std::string& type, const std::string& name);
        void RemapBones(const std::vector<Node*>& made, const PtrList* theirs);
        void StockEffects();
        void DropSwappedEffects(std::vector<std::pair<Entry*, Entry*>> swapped);
        void StockModelSwap();
        void StockAnims();
        void AddBreakables();
        int PaintBreakable(Struct* model, const std::optional<ColourSwap>& swap,
                           const Ordered<std::string, Struct*>& ours);

        // ---- sounds (sounds.cpp) -------------------------------------------
        void AddDestructibleAliases();
        void StockSounds();
        void NameLoadedCurves(Struct* alias);
        // A PC alias head that plays a sound found here, and the sound file's
        // name in lower case.
        struct HeadAudio
        {
            Struct* head;
            std::string sound;
        };
        // The heads of a PC alias list whose audio is here (the map's loaded
        // sounds, streamed ones in the .iwd files); each sound is encoded once.
        std::vector<HeadAudio> PcAudio(Struct* alias);
        Xma Encode(const std::string& name, const Pcm& pcm);
        void EncodedHeads(Struct* alias, const std::vector<HeadAudio>& audio);
        // The sound/ files of the PC game's .iwd files, then the map's (later
        // ones win), by path under sound/ in lower case.
        const ZipIndex& SoundFiles();

        // ---- materials and shader sets (materials.cpp) ---------------------
        bool PreMaterialTechniqueSet(Struct* d);
        bool PreMaterial(Struct* d);
        void PostMaterial(Struct* d);
        void FillTextures(Struct* d, Struct* tpl, const std::string& name, bool invisible);
        bool HaveTechset(const std::string& name) const;
        std::optional<std::string> NearestTechset(const std::string& name);
        bool IsToolsTechset(const std::string& name) const;
        std::optional<std::string> InvisibleTechset(const std::string& name) const;
        bool Hides(const std::string& name, const std::string& near) const;
        std::optional<std::string> SimilarTechset(const std::string& name) const;
        std::optional<bool> TechsetHas(const std::string& name, int technique = 9);
        std::optional<uint32_t> PcCullOf(Struct* d);

        // ---- pictures (pictures.cpp) ---------------------------------------
        bool PreGfxImage(Struct* d);
        bool PictureFromFiles(Struct* d, const std::string& name);
        void ImageFromIwd(Struct* d, const std::string& name, const std::vector<uint8_t>& data);
        void ImageFromLoadDef(Struct* d, Struct* loadDef);
        void BuildImage(Struct* d, Gpu f, int width, int height, const std::vector<std::vector<Mip>>& faces, bool cube,
                        Struct* tpl = nullptr);
        Struct* Magenta(Struct* d, std::optional<std::string> name = std::nullopt);
        Struct* SeeThrough(Struct* d, const std::string& name);
        static std::optional<std::string> StandIn(const Struct* d, const std::string& name);
        Struct* WaterImage(Struct* d, const std::string& name);
        void ProbeAlpha(Struct* d);
        bool InIwd(const std::string& name) const;
        bool StreamsFromStock(const std::string& name) const;

        // ---- the world (world.cpp) -----------------------------------------
        bool PreGfxWorldDpvsDynamic(Struct* d, Struct* x);
        bool PreGfxStaticModelDrawInst(Struct* d, Struct* x);
        bool PreWaterT(Struct* d, Struct* x);
        void PostGfxWorld(Struct* d);
        void PostGfxWorldDraw(Struct* d);
        void PostGfxWorldDpvsStatic(Struct* d);
        void PostGfxAabbTree(Struct* d);
        void PostGfxLightGrid(Struct* d);
        void PostFxElemVisualState(Struct* d);
        void PostFxGlassDef(Struct* d);
        void PostFxGlassSystem(Struct* d);
        void PostClipMap(Struct* d);
        void PostMapEnts(Struct* d);
        void PostXSurface(Struct* d);
        void PostXModel(Struct* d);
        void FixBoneBounds(Struct* d);
        void OrderSurfaces(Struct* world);
        std::vector<size_t> SolidRuns(const std::vector<Node*>& surfs, const std::vector<size_t>& order,
                                      const std::vector<int>& groups, const std::vector<int64_t>& sortKeys);
        void FillSurfaceBounds(Struct* world);
        void GrowTreeBounds(Struct* world);
        void FillLightingOrigins(Struct* world);
        void SliceTreeLists(Struct* world);
        void DimStandinProbes(Struct* world);
        void WorldMaterialMemory(Struct* world);
        // (decals.cpp)
        void MergeDecals(Struct* world);
        void RelaySurfaces(Struct* world);
        void CompactVertices(Struct* world);

        // ---- teams (teams.cpp) ---------------------------------------------
        void PickCardPictures();
        void Pick(StockFile* src, const std::set<std::pair<std::string, std::string>>& names);
        void CopyPicked(StockFile* src);

        friend class DecalMerger;

        Tree& root;
        std::vector<StockFile*> refs;
        Log log;
        int textureBudget;
        std::vector<Zip> iwds;
        const std::vector<Zip>& gameIwds;
        // The pictures of the map's .iwd files and of the PC game's.
        ZipIndex mapPictures, gamePictures;
        int fromGame = 0;   // pictures taken from the PC game's files

        // Stock 360 assets: those the game has loaded (named only, ",name"),
        // those to copy in, and those stock files only name.
        AssetIndex<Struct*> resident, library, commonMaterials;
        std::set<std::pair<std::string, std::string>> named;
        std::set<std::pair<std::string, std::string>> loaded;        // (type, lower name) the resident files have
        std::set<std::pair<std::string, std::string>> commonNames;   // (type, lower name) common_mp has
        struct StockAsset
        {
            Struct* asset;
            StockFile* file;
        };
        Ordered<std::string, StockAsset> stockFx, stockModels;
        struct StockAlias
        {
            Struct* alias;
            bool resident;
            StockFile* file;
            int64_t order;
        };
        Ordered<std::string, StockAlias> stockAliases;
        Ordered<std::string, Struct*> stockStreamed;     // lower-case picture name -> a stock streamed one
        std::map<std::pair<int, bool>, Struct*> templates;   // (gpu format, cube) -> a stock picture
        Ordered<std::string, Struct*> lightmapTemplates;
        Ordered<std::string, Struct*> materialTemplates;      // shader set name -> a stock material using it

        std::unordered_set<const Node*> done;
        std::unordered_map<std::string, int> mipDrop;
        Ordered<std::string, std::string> techsetSwaps;
        struct MissingImage
        {
            std::string name;
            std::optional<std::string> standIn;   // a built-in picture, else magenta
        };
        std::vector<MissingImage> missingImages;
        std::vector<std::string> unreadable, magentaMaterials, invisibleMaterials;
        std::unordered_set<const Node*> invisibleTs;
        std::vector<Struct*> stockFxUsed;
        std::unordered_map<const Node*, Entry*> modelSwap;
        std::unordered_set<const Node*> swappedRows;
        std::unordered_map<const Node*, int64_t> pcSort;
        std::unordered_map<const Node*, uint32_t> pcCull;
        std::unordered_map<const Node*, Struct*> materialTemplate;   // a material being converted -> its template
        std::unordered_set<const Node*> invisible;
        int twoSided = 0, dxnCount = 0, streamedStock = 0;
        std::optional<std::map<std::string, std::array<uint32_t, 2>>> stockGlass;   // material (and texture scale) -> numbers
        std::optional<std::map<int64_t, std::set<int>>> stockSortKeys;   // sort key -> {lit: 1, unlit: 0, unknown: -1}
        struct SortKeySwap
        {
            std::string material;
            int64_t pc, stock;
        };
        std::vector<SortKeySwap> sortKeyKindSwapped;
        // The map's own sounds, encoded: by PC sound file name, lower case.
        struct EncodedSound
        {
            std::optional<Xma> xma;
            bool resident = false;      // the game has it loaded: only named
            Struct* first = nullptr;    // the 360 sound made of it, once made
            bool Plays() const { return xma || resident; }
        };
        std::unordered_map<std::string, EncodedSound> encoded;
        std::unordered_map<std::string, Struct*> pcLoaded;     // the map's loaded sounds by name, lower case
        std::optional<ZipIndex> soundFiles;
        std::set<std::string> noAudio;                         // sound files with no audio found
        size_t encodedBytes = 0;
        int encodeCount = 0;
        double encodeSeconds = 0;
        bool compressedSounds = false;                         // some aren't 16-bit PCM
        std::optional<std::unordered_map<std::string, std::vector<Node*>>> stockTechniques;
        std::map<std::pair<std::string, int>, std::optional<bool>> techsetHasCache;
        std::vector<Entry*> extraAssets;
        std::vector<Struct*> movedImages;
        std::unordered_map<const StockFile*, std::set<size_t>> picked;
        std::unordered_map<const StockFile*, std::unordered_map<const Node*, size_t>> owners;
        std::unordered_set<const Node*> stockEntries;
        std::unordered_set<const Node*> remapped;
        Ordered<std::string, std::string> ambientSwaps;
        std::set<std::string> scriptWords;
        int64_t worldChecksum = 0;
        int boneBounds = 0, groundLit = 0;
        std::optional<std::pair<Ordered<std::string, Struct*>, std::unordered_map<const Node*, StockFile*>>> stockAnims;
    };

    // ---- the world's data (world.cpp, decals.cpp) ---------------------------

    constexpr size_t kVertexBytes = 44;   // a GfxWorldVertex
    // The name hashes of a material's colour pictures (colorMap, colorMap1).
    constexpr uint32_t kColorMap = 0xA0AB1041, kColorMap1 = 0xB60D1850;
    // 16-bit numbers (indices) as plain data in a byte order.
    std::vector<uint8_t> Pack16(const std::vector<uint32_t>& v, bool big);

    // A lit world shader set's name as a composite decal set's part
    // (decals.cpp): "wc_l_sm_r0c0n0s0p0" -> "l_sm_r0c0n0s0" and its "p0", and
    // a decal's set as layer i's part.
    struct TechsetCode
    {
        std::string code;
        bool p0;
    };
    std::optional<TechsetCode> GroundCode(const std::string& techset);
    std::optional<TechsetCode> LayerCode(const std::string& techset, size_t layer);

    // The stock maps' arena file (mp/basemaps.arena): each map's teams
    // (teams.cpp).
    std::vector<std::string> TeamMaps(const std::string& arena, const std::string& team);
    std::optional<Teams> ArenaTeams(const std::string& arena, const std::string& map);

    // The names an asset list's assets take a place in the game's pools
    // under, lower case, by type ("name" references take none), and the room
    // each pool has (port.cpp).
    std::map<std::string, std::set<std::string>> PoolNames(const std::vector<Entry*>& assets);
    int PoolLimit(std::string_view type);

    // The pointer value a Ref into an object of another layout keeps once
    // converted: its target's member by name.
    int64_t MapRel(const Ref* r, const zone::Schema& X);
}
