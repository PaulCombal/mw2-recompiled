# Notice

This converter is a port of pc2xbox360mw2ools
(https://github.com/slanginbeans/pc2xbox360mw2ools, commit 79560e9), licensed under the GNU
General Public License v3.0: its `mw2ff/` (`port.py`, `decals.py`, `tree.py`, `zone.py`,
`wavelet.py`, `xma.py`) and the picture code of `mw2tex/mw2tex.py`. It does what that tool does with its
default fixes, and writes the same files, byte for byte.

The wavelet picture decoder follows OpenAssetTools' `IwiWaveletDecoder` (the iw4x-x64/oat fork,
https://github.com/iw4x/oat), as pc2xbox360mw2ools' `wavelet.py` does; its codebooks are that
decoder's tables. OpenAssetTools is licensed under the GNU General Public License v3.0.

The picture resizing reproduces Pillow's (https://python-pillow.org, the HPND licence): its BC
decoder, its premultiplied-alpha Lanczos resampling and its rounding.

The XMA encoder (`src/xma.rs`, from `xma.py`) writes the bitstream FFmpeg's WMA Pro decoder
(libavcodec/wmaprodec.c) reads, and its code tables are FFmpeg's (libavcodec/wmaprodata.h, n6.1.1:
Copyright (c) 2007 Baptiste Coudurier, Benjamin Larsson, Ulion; (c) 2008 - 2009 Sascha Sommer,
Benjamin Larsson), licensed under the GNU Lesser General Public License, version 2.1 or later.

The fastfiles' deflate (`src/deflate.rs`) makes what zlib 1.3.1's compress2 makes
(https://zlib.net, Jean-loup Gailly and Mark Adler, the zlib licence): its lazy matching and
Huffman trees, followed step by step.
