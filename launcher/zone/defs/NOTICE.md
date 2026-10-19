# Notice

These files are copied from pc2xbox360mw2ools, unchanged but for their line endings
(https://github.com/slanginbeans/pc2xbox360mw2ools, `mw2ff/defs/`, commit 79560e9), which is
licensed under the GNU General Public License v3.0.

They are adapted there from OpenAssetTools (https://github.com/Laupetin/OpenAssetTools), files
`src/Common/Game/IW4/IW4_Assets.h` and `src/ZoneCode/Game/IW4/`, also licensed under the GNU
General Public License v3.0. pc2xbox360mw2ools changed them for the Xbox 360 title update 6
layouts and load order of shaders, vertex declarations, materials, techniques, images, menus,
models and model surfaces, sounds, animations, the sound driver globals asset, map world,
collision, glass and water data.

`iw4_assets.h` describes the structures; `commands.txt` and `assets/*.txt` say how the game
streams them out of a zone. `launcher/zone/` reads them as they are.
