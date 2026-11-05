# Notice

This folder is a Rust port of the part of FFmpeg that decodes XMA frames: the `xmaframes`
codec of the FFmpeg branch Xenia carries (https://github.com/has207/FFmpeg, commit d980192),
which is libavcodec/wmaprodec.c's WMA Pro frame decoder fed one XMA frame at a time, with what it
uses of libavcodec (the bit reader of get_bits.h, the VLC tables of vlc.c, wma.c's run-level
decoding, the sine windows of sinewin_tablegen.h) and of libavutil (the inverse MDCT and
split-radix FFT of tx.c and tx_template.c, and float_dsp.c's C routines). It does what that code
does, in the same order, so that it gives the same samples bit for bit.

The files it was ported from carry these notices, which go with the port:

- wmaprodec.c and wmaprodata.h (mod.rs, tables.rs): Copyright (c) 2007 Baptiste Coudurier,
  Benjamin Larsson, Ulion; Copyright (c) 2008 - 2009 Sascha Sommer, Benjamin Larsson.
- wma.c (mod.rs's run-level decoding): Copyright (c) 2002-2007 The FFmpeg Project.
- get_bits.h (bits.rs): Copyright (c) 2004 Michael Niedermayer.
- vlc.c (vlc.rs): Copyright (c) 2000, 2001 Fabrice Bellard; Copyright (c) 2002-2004 Michael
  Niedermayer; Copyright (c) 2010 Loren Merritt.
- tx.c and tx_template.c (mdct.rs): Copyright (c) Lynne; Copyright (c) 2008 Loren Merritt;
  Copyright (c) 2002 Fabrice Bellard.
- float_dsp.c (mod.rs's windowing and scaling): Copyright 2005 Balatoni Denes; Copyright 2006
  Loren Merritt.
- sinewin_tablegen.h (mod.rs's sine windows): Copyright (c) 2009 Reimar Döffinger.

FFmpeg is licensed under the GNU Lesser General Public License, version 2.1 or later, and so is
this port (LICENSE.LGPL-2.1); the game it is part of is GPL 3.0, which that licence allows.
