# Custom maps: converting a PC map

A map made for the PC game (IW4x, or the 2009 PC game: fastfile version 276) cannot be loaded by
the 360's multiplayer as it is: its structures are laid out for another compiler, its numbers
are little-endian, its shaders are PC shaders, its pictures are not tiled for the console's GPU,
and it leaves out what the PC game loads from its own files. The launcher converts one when it
is added (MAPS → ADD MAP, or `mw2-launcher --add-map`), with `launcher/mapport/` (its
`NOTICE.md` credits the work it follows). What the game then does with a converted map is in
[gameplay.md](gameplay.md#game-data).

## Inputs

- the PC map: `<map>.ff`, and `<map>_load.ff` (its loading screen), `<map>.iwd` (a zip of its
  `.iwi` pictures) and `<map>.arena` (its names and teams) when they come with it;
- the game's own fastfiles in `game/`, but for the custom maps installed there:
  `code_post_gfx_mp.ff` and `common_mp.ff`, which the game always has loaded, and the stock maps,
  which the conversion takes shader sets, render state, effects, sounds, animations, models and
  the teams from;
- the PC game's `.iwd` files, when the player has the PC game: a map made for it may use the
  pictures and sounds the game's own maps share without carrying them. The launcher takes the
  folder `MW2_PC_GAME` names (MAPS → PC GAME), else Steam's copy, which it writes there: the
  first Steam installation that has the game (its usual folders, the snap's and the Flatpak's;
  on Windows the registry's), in Steam's own folder or a library its `libraryfolders.vdf` names,
  at the folder the game's app manifest gives. Every `.iwd` in that folder and its subfolders but
  `usermaps/` and `mods/` is read, in the order of their paths (`GameIwds`). What the map's own
  `.iwd` has comes from it, the rest from there. Only the `.iwd` files are read: Steam's game was rebuilt for 64 bits in
  2026, and its fastfiles are not laid out as a 2009 PC map's.

## The tree

`reader.h` reads a zone as the game's loader streams it, by the zone definitions
(`launcher/zone/defs/` for the 360, `launcher/zone/defs_pc/` for the PC), into a tree of nodes
(`tree.h`):

- a `Struct` holds its bytes in its platform's layout; an embedded struct is a view of its
  owner's bytes; its pointers' targets are its `Children`;
- a pointer back to earlier data is a `Ref` (target, offset in it, element type); the writer
  works its value out anew from where the target was written;
- a temp-block asset's `Slot` is the pointer the loader makes for it, which later pointers name.

Nodes have identity: steps compare and mark nodes as themselves, and the order a tree is walked
in (`ForEach`: depth first, a struct's embedded members, then its children) decides the order
assets go into the file. The writer (`TreeWriter`) streams a tree back out; written with
`keepBytes`, a tree as read gives its zone back unchanged (`map-port roundtrip`), for every
stock multiplayer fastfile but `common_mp.ff`, whose animation data is read only roughly
(`lenient`): the conversion only looks things up in it.

## The conversion

`Porter` (`porter.h`) turns the PC tree into the 360's, then `port.cpp` writes it:

- **Structs** (`convert.cpp`): each is rebuilt as the 360's struct of the same name, member by
  member, numbers big-endian; hooks handle what the platforms keep differently: world and model
  vertices (normals and tangents repacked to 10:10:10), placed models (packed axes), the culling
  trees' child offsets, the light grid's row headers, effect colours, water, glass.
- **Shader sets and materials** (`materials.cpp`): PC shaders cannot run on the 360, so a shader
  set is swapped for the 360's own of the same name, or the nearest the stock files have; a
  material's render state comes from a stock material using that set; its draw order and face
  culling stay the PC's.
- **Pictures** (`pictures.cpp`, `images.h`): read from the `.iwd` (DXT, plain, or wavelet:
  `wavelet.h`), resized when the 360 cannot take them, normal maps made DXN, then tiled. A
  picture a stock map streams from the console's picture packs is taken from it instead.
  The map's pictures are counted as the 360 tiles them (but its loading screens, and those the
  stock files stream or the game has loaded); over 40 MB, the largest picture with mip levels
  loses its top one, again until they fit. A shine map counts double, so it goes first: its
  loss shows least. Lightmaps, reflection probes and cube maps count, but keep their levels.
- **The world** (`world.cpp`, `decals.cpp`): decal layers merged into the ground under them as
  the 360's compiler does (composite materials), the surfaces put in the 360's order, their
  bounds and the culling trees' boxes filled in.
- **From the stock files** (`stock.cpp`, `teams.cpp`): effects, animations, models and scripts
  a stock map has under the same name, the breakable parts the map's destructibles need, and the
  two teams' soldiers, flags and icons, which the 360 keeps in each map's file. A copy is made to
  stand alone (`Localize`): pointers back into the stock file are replaced by what they point
  at.
- **Sounds** (`sounds.cpp`, `xma.h`): an alias list a stock file has comes from it, audio and
  all. One no stock file has keeps the PC's numbers and plays the PC's audio, encoded as XMA,
  the only format the 360 game plays: a sound the map loads, or one the PC streams, from the
  map's or the PC game's `.iwd`, which becomes a loaded one (the 360 streams only from its disc).
  The encoder writes the subset of WMA Pro FFmpeg's decoder reads: 512-sample frames, one MDCT
  each, per-band scale factors, run-level coded coefficients, in 2,048-byte packets. A sound the
  game has loaded already is only named; one without audio gets the stock "null" alias's
  silence.

Stock maps not among the files the conversion takes from are scanned too, for effects and shader
sets the map uses and for the pictures they stream; they are read several at a time.

The file is written as an unsigned 360 fastfile (`fastfile.h`): the pak table of the stock
pictures it streams, then the zone, deflated.

The converted loading screen is written a second time as `<map>_preview.ff`, the picture the
game's menus show of the map: its `$levelbriefing` material alone, named `preview_<map>`,
as a stock map's picture is, and laid out as the downloadable maps' are in `dlc1_ui_mp.ff`
(a 2D material on the `,2d` shader set, one picture of one level). PC maps carry no such
picture; IW4x's arenas name one all the same (`mapimage`).

## Limits

- Sounds in a format other than 16-bit PCM (`.mp3`, ADPCM `.wav`) stay silent: they are not
  decoded. So do the sounds past the first 12 MB encoded (stock maps carry 4 to 8 MB of sounds),
  and an ambience track the 360's own replaces.
- Animations no stock file has are left out.
- Pictures the map takes from the PC game's own files, without the PC game: magenta stands in, or
  a flat normal map, or no shine. With it, a map can still name a picture neither has (one from
  another mod or game).
- A map's script can call what only later PC patches have; a stock map's own script replaces the
  PC's when there is one.

## The tool

`map-port` (built with the launcher) runs the converter and its checks from a
terminal:

    map-port port <map.ff> <out dir> <stock dir> [PC game folder]
                                                   convert, the stock files from a folder
    map-port roundtrip <file.ff>...                read and write back: the same bytes
    map-port canonical <file.ff> <zone>            write as converted files are written
    map-port list <file.ff>                        the asset list
    map-port iwi <file.iwd> <picture> <out>        a picture as the converter reads it
    map-port fit <file.iwd> <picture> <limit> <out> a picture as the converter resizes it
    map-port xma <file.wav> <out>                  a sound encoded as the converter encodes it

`MAP_PORT_TIMES=1` puts the time before each line of `port`'s log.
