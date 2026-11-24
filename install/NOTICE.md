# Notice

Two files of this crate are ports of other projects' code and keep their licences.

## src/lzx.rs: libmspack, LGPL 2.1

src/lzx.rs is a Rust port of libmspack's LZX decompressor: `mspack/lzxd.c`, with what it uses of
`lzx.h`, `readbits.h` (the bit reader) and `readhuff.h` (the Huffman decoder), from the copy
XenonRecomp-rs carries (`thirdparty/libmspack`, libmspack commit 3059077). It does what that code
does, in the same order, so that it decodes the same bytes and refuses the same streams.

Those files carry this notice, which goes with the port: (C) 2003-2023 Stuart Caie. The LZX method
was created by Jonathan Forbes and Tomi Poutanen, adapted by Microsoft Corporation. readhuff.h's
`make_decode_table()` was originally coded by David Tritscher.

libmspack is licensed under the GNU Lesser General Public License, version 2.1, and so is this port
(LICENSE.LGPL-2.1); the game and the launcher it is part of are GPL 3.0, which that licence allows.

## src/patch.rs: XenonRecomp's patcher, MIT, after Xenia's, BSD

src/patch.rs is a Rust port of XenonRecomp's executable patcher (`XenonUtils/xex_patcher.cpp` in
XenonRecomp-rs), which follows Xenia's (`src/xenia/cpu/xex_module.cc`). Its notices:

> MIT License
>
> Copyright (c) 2025 hedge-dev and contributors
>
> Permission is hereby granted, free of charge, to any person obtaining a copy of this software and
> associated documentation files (the "Software"), to deal in the Software without restriction,
> including without limitation the rights to use, copy, modify, merge, publish, distribute,
> sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is
> furnished to do so, subject to the following conditions:
>
> The above copyright notice and this permission notice shall be included in all copies or
> substantial portions of the Software.
>
> THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT
> NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
> NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM,
> DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT
> OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

> Copyright (c) 2015, Ben Vanik. All rights reserved.
>
> Redistribution and use in source and binary forms, with or without modification, are permitted
> provided that the following conditions are met:
>
> - Redistributions of source code must retain the above copyright notice, this list of conditions
>   and the following disclaimer.
> - Redistributions in binary form must reproduce the above copyright notice, this list of
>   conditions and the following disclaimer in the documentation and/or other materials provided
>   with the distribution.
> - Neither the name of the project nor the names of its contributors may be used to endorse or
>   promote products derived from this software without specific prior written permission.
>
> THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY EXPRESS OR
> IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND
> FITNESS FOR A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL BEN VANIK BE LIABLE FOR ANY
> DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT
> LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR
> BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT
> LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS
> SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

The rest of the crate is the project's, GPL 3.0.
