// RFC 1950's zlib stream of RFC 1951's deflate, made as zlib 1.3.1's compress2 makes it at
// levels 4 to 9 (its lazy matching, deflate_slow, with the default window, memory level
// and strategy), so that a converted map's fastfile comes out the same bytes as zlib
// would make of its zone: the same hash chains and window, the same matches taken, the
// same blocks ended at the same symbols, and each block stored, or coded with the fixed
// or its own Huffman codes, as zlib chooses.

const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;
const MIN_LOOKAHEAD: usize = MAX_MATCH + MIN_MATCH + 1;
const W_BITS: u32 = 15;
const W_SIZE: usize = 1 << W_BITS;
const W_MASK: usize = W_SIZE - 1;
const WINDOW_SIZE: usize = 2 * W_SIZE;
const MAX_DIST: usize = W_SIZE - MIN_LOOKAHEAD;
const MEM_LEVEL: u32 = 8;
const HASH_BITS: u32 = MEM_LEVEL + 7;
const HASH_SIZE: usize = 1 << HASH_BITS;
const HASH_MASK: u32 = HASH_SIZE as u32 - 1;
const HASH_SHIFT: u32 = HASH_BITS.div_ceil(MIN_MATCH as u32);
const LIT_BUFSIZE: usize = 1 << (MEM_LEVEL + 6);
// Each symbol takes three bytes: the distance (0 for a literal), then the literal or the
// length.
const SYM_END: usize = (LIT_BUFSIZE - 1) * 3;
// Bytes past the data in the window that are zeroed before the matcher may read them.
const WIN_INIT: usize = MAX_MATCH;
// Matches of length 3 are discarded if their distance exceeds this.
const TOO_FAR: usize = 4096;

const LENGTH_CODES: usize = 29;
const LITERALS: usize = 256;
const L_CODES: usize = LITERALS + 1 + LENGTH_CODES;
const D_CODES: usize = 30;
const BL_CODES: usize = 19;
const HEAP_SIZE: usize = 2 * L_CODES + 1;
const MAX_BITS: usize = 15;
const MAX_BL_BITS: usize = 7;
const END_BLOCK: usize = 256;
const REP_3_6: usize = 16;
const REPZ_3_10: usize = 17;
const REPZ_11_138: usize = 18;

const EXTRA_LBITS: [u32; LENGTH_CODES] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const EXTRA_DBITS: [u32; D_CODES] =
    [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];
const EXTRA_BLBITS: [u32; BL_CODES] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 3, 7];
// The order the bit length codes' lengths are sent in.
const BL_ORDER: [usize; BL_CODES] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];

// A code's two words: its frequency while the tree is built, then its code; its parent
// in the tree, then its length. Each pair shares a word, as the build relies on.
#[derive(Clone, Copy, Default)]
struct Ct {
    fc: u16,
    dl: u16,
}

// The fixed codes and the tables of lengths and distances.
struct Static {
    ltree: [Ct; L_CODES + 2],
    dtree: [Ct; D_CODES],
    dist_code: [u8; 512],
    length_code: [u8; MAX_MATCH - MIN_MATCH + 1],
    base_length: [u32; LENGTH_CODES],
    base_dist: [u32; D_CODES],
}

fn bi_reverse(mut code: u32, mut len: u32) -> u32 {
    let mut res = 0;
    loop {
        res |= code & 1;
        code >>= 1;
        res <<= 1;
        len -= 1;
        if len == 0 {
            return res >> 1;
        }
    }
}

// Each code of the tree with a length, its code, from how many codes each length has.
fn gen_codes(tree: &mut [Ct], max_code: isize, bl_count: &[u16; MAX_BITS + 1]) {
    let mut next_code = [0u16; MAX_BITS + 1];
    let mut code: u32 = 0;
    for bits in 1..=MAX_BITS {
        code = (code + u32::from(bl_count[bits - 1])) << 1;
        next_code[bits] = code as u16;
    }
    for n in 0..=max_code.max(-1) {
        let n = n as usize;
        let len = tree[n].dl as usize;
        if len == 0 {
            continue;
        }
        tree[n].fc = bi_reverse(u32::from(next_code[len]), len as u32) as u16;
        next_code[len] = next_code[len].wrapping_add(1);
    }
}

impl Static {
    fn new() -> Static {
        let mut s = Static {
            ltree: [Ct::default(); L_CODES + 2],
            dtree: [Ct::default(); D_CODES],
            dist_code: [0; 512],
            length_code: [0; MAX_MATCH - MIN_MATCH + 1],
            base_length: [0; LENGTH_CODES],
            base_dist: [0; D_CODES],
        };
        let mut length = 0usize;
        let mut code = 0;
        while code < LENGTH_CODES - 1 {
            s.base_length[code] = length as u32;
            for _ in 0..1 << EXTRA_LBITS[code] {
                s.length_code[length] = code as u8;
                length += 1;
            }
            code += 1;
        }
        // The length 255 (match length 258) can be represented two ways: code 284 + 5
        // bits, or code 285; the last one is used.
        s.length_code[length - 1] = code as u8;
        let mut dist = 0usize;
        code = 0;
        while code < 16 {
            s.base_dist[code] = dist as u32;
            for _ in 0..1 << EXTRA_DBITS[code] {
                s.dist_code[dist] = code as u8;
                dist += 1;
            }
            code += 1;
        }
        dist >>= 7; // from now on, all distances are divided by 128
        while code < D_CODES {
            s.base_dist[code] = (dist << 7) as u32;
            for _ in 0..1 << (EXTRA_DBITS[code] - 7) {
                s.dist_code[256 + dist] = code as u8;
                dist += 1;
            }
            code += 1;
        }
        let mut bl_count = [0u16; MAX_BITS + 1];
        for (n, c) in s.ltree.iter_mut().enumerate() {
            let len = match n {
                0..=143 => 8,
                144..=255 => 9,
                256..=279 => 7,
                _ => 8,
            };
            c.dl = len;
            bl_count[len as usize] += 1;
        }
        // Codes 286 and 287 do not exist, but are in the construction of a complete code.
        gen_codes(&mut s.ltree, (L_CODES + 1) as isize, &bl_count);
        for (n, c) in s.dtree.iter_mut().enumerate() {
            c.dl = 5;
            c.fc = bi_reverse(n as u32, 5) as u16;
        }
        s
    }

    fn d_code(&self, dist: usize) -> usize {
        usize::from(if dist < 256 { self.dist_code[dist] } else { self.dist_code[256 + (dist >> 7)] })
    }
}

// Which tree, for the tree builder.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Which {
    Literals,
    Distances,
    BitLengths,
}

struct State {
    st: Static,
    out: Vec<u8>,
    bi_buf: u16,
    bi_valid: u32,

    window: Vec<u8>,
    prev: Vec<u16>,
    head: Vec<u16>,
    ins_h: u32,
    high_water: usize,
    block_start: i64,
    strstart: usize,
    match_start: usize,
    lookahead: usize,
    prev_length: usize,
    match_length: usize,
    prev_match: usize,
    match_available: bool,
    insert: usize,
    max_chain_length: u32,
    max_lazy_match: usize,
    good_match: usize,
    nice_match: usize,

    dyn_ltree: [Ct; HEAP_SIZE],
    dyn_dtree: [Ct; 2 * D_CODES + 1],
    bl_tree: [Ct; 2 * BL_CODES + 1],
    l_max_code: isize,
    d_max_code: isize,
    bl_max_code: isize,
    bl_count: [u16; MAX_BITS + 1],
    heap: [usize; HEAP_SIZE],
    heap_len: usize,
    heap_max: usize,
    depth: [u8; HEAP_SIZE],
    sym_buf: Vec<u8>,
    sym_next: usize,
    opt_len: u64,
    static_len: u64,
}

impl State {
    // ---- bits out -------------------------------------------------------------------------

    fn put_short(&mut self, w: u16) {
        self.out.push(w as u8);
        self.out.push((w >> 8) as u8);
    }

    fn send_bits(&mut self, value: u32, length: u32) {
        if self.bi_valid > 16 - length {
            self.bi_buf |= (value << self.bi_valid) as u16;
            let b = self.bi_buf;
            self.put_short(b);
            self.bi_buf = ((value as u16) as u32 >> (16 - self.bi_valid)) as u16;
            self.bi_valid = self.bi_valid + length - 16;
        } else {
            self.bi_buf |= (value << self.bi_valid) as u16;
            self.bi_valid += length;
        }
    }

    fn bi_windup(&mut self) {
        if self.bi_valid > 8 {
            let b = self.bi_buf;
            self.put_short(b);
        } else if self.bi_valid > 0 {
            self.out.push(self.bi_buf as u8);
        }
        self.bi_buf = 0;
        self.bi_valid = 0;
    }

    // ---- trees ----------------------------------------------------------------------------

    fn init_block(&mut self) {
        for c in &mut self.dyn_ltree[..L_CODES] {
            c.fc = 0;
        }
        for c in &mut self.dyn_dtree[..D_CODES] {
            c.fc = 0;
        }
        for c in &mut self.bl_tree[..BL_CODES] {
            c.fc = 0;
        }
        self.dyn_ltree[END_BLOCK].fc = 1;
        self.opt_len = 0;
        self.static_len = 0;
        self.sym_next = 0;
    }

    fn tree(&mut self, which: Which) -> &mut [Ct] {
        match which {
            Which::Literals => &mut self.dyn_ltree,
            Which::Distances => &mut self.dyn_dtree,
            Which::BitLengths => &mut self.bl_tree,
        }
    }

    fn smaller(tree: &[Ct], depth: &[u8], n: usize, m: usize) -> bool {
        tree[n].fc < tree[m].fc || (tree[n].fc == tree[m].fc && depth[n] <= depth[m])
    }

    // Restores the heap property from k down, moving down the tree.
    fn pqdownheap(&mut self, which: Which, mut k: usize) {
        let tree: &[Ct] = match which {
            Which::Literals => &self.dyn_ltree,
            Which::Distances => &self.dyn_dtree,
            Which::BitLengths => &self.bl_tree,
        };
        let v = self.heap[k];
        let mut j = k << 1;
        while j <= self.heap_len {
            if j < self.heap_len && State::smaller(tree, &self.depth, self.heap[j + 1], self.heap[j]) {
                j += 1;
            }
            if State::smaller(tree, &self.depth, v, self.heap[j]) {
                break;
            }
            self.heap[k] = self.heap[j];
            k = j;
            j <<= 1;
        }
        self.heap[k] = v;
    }

    // Whether the tree has fixed codes (and whether those are the literals'), its extra
    // bits, the first code with extra bits, its element count and its longest code.
    fn desc(which: Which) -> (Option<bool>, &'static [u32], usize, usize, usize) {
        match which {
            Which::Literals => (Some(true), &EXTRA_LBITS, LITERALS + 1, L_CODES, MAX_BITS),
            Which::Distances => (Some(false), &EXTRA_DBITS, 0, D_CODES, MAX_BITS),
            Which::BitLengths => (None, &EXTRA_BLBITS, 0, BL_CODES, MAX_BL_BITS),
        }
    }

    fn static_len_of(&self, which: Which, n: usize) -> Option<u16> {
        match which {
            Which::Literals => Some(self.st.ltree[n].dl),
            Which::Distances => Some(self.st.dtree[n].dl),
            Which::BitLengths => None,
        }
    }

    // The optimal bit lengths of a tree whose frequencies and parents are set, at most
    // the tree's longest; opt_len and static_len updated.
    fn gen_bitlen(&mut self, which: Which, max_code: isize) {
        let (_, extra, base, _, max_length) = State::desc(which);
        self.bl_count = [0; MAX_BITS + 1];
        let mut overflow = 0;
        let root = self.heap[self.heap_max];
        self.tree(which)[root].dl = 0; // root of the heap
        let mut h = self.heap_max + 1;
        while h < HEAP_SIZE {
            let n = self.heap[h];
            let tree = self.tree(which);
            let mut bits = tree[tree[n].dl as usize].dl as usize + 1;
            if bits > max_length {
                bits = max_length;
                overflow += 1;
            }
            tree[n].dl = bits as u16;
            h += 1;
            if n as isize > max_code {
                continue; // not a leaf node
            }
            let f = u64::from(tree[n].fc);
            self.bl_count[bits] += 1;
            let xbits = if n >= base { extra[n - base] as usize } else { 0 };
            self.opt_len = self.opt_len.wrapping_add(f * (bits + xbits) as u64);
            if let Some(len) = self.static_len_of(which, n) {
                self.static_len = self.static_len.wrapping_add(f * (usize::from(len) + xbits) as u64);
            }
        }
        if overflow == 0 {
            return;
        }
        // Find the first bit length which could increase.
        loop {
            let mut bits = max_length - 1;
            while self.bl_count[bits] == 0 {
                bits -= 1;
            }
            self.bl_count[bits] -= 1; // move one leaf down the tree
            self.bl_count[bits + 1] += 2; // move one overflow item as its brother
            self.bl_count[max_length] -= 1;
            // The brother of the overflow item also moves one step up, but this does not
            // affect bl_count[max_length].
            overflow -= 2;
            if overflow <= 0 {
                break;
            }
        }
        // Recompute all bit lengths, scanning in increasing frequency.
        let mut h = HEAP_SIZE;
        for bits in (1..=max_length).rev() {
            let mut n = self.bl_count[bits];
            while n != 0 {
                h -= 1;
                let m = self.heap[h];
                if m as isize > max_code {
                    continue;
                }
                let tree = self.tree(which);
                if usize::from(tree[m].dl) != bits {
                    let delta = (bits as u64).wrapping_sub(u64::from(tree[m].dl)).wrapping_mul(u64::from(tree[m].fc));
                    tree[m].dl = bits as u16;
                    self.opt_len = self.opt_len.wrapping_add(delta);
                }
                n -= 1;
            }
        }
    }

    // A Huffman tree of the frequencies, and its codes.
    fn build_tree(&mut self, which: Which) {
        let (has_static, _, _, elems, _) = State::desc(which);
        let mut max_code: isize = -1;
        self.heap_len = 0;
        self.heap_max = HEAP_SIZE;
        for n in 0..elems {
            if self.tree(which)[n].fc != 0 {
                self.heap_len += 1;
                self.heap[self.heap_len] = n;
                max_code = n as isize;
                self.depth[n] = 0;
            } else {
                self.tree(which)[n].dl = 0;
            }
        }
        // The pkzip format requires that at least one distance code exists, and that at
        // least one bit should be sent even if there is only one possible code. So to
        // avoid special checks later on we force at least two codes of non zero
        // frequency.
        while self.heap_len < 2 {
            let node = if max_code < 2 {
                max_code += 1;
                max_code as usize
            } else {
                0
            };
            self.heap_len += 1;
            self.heap[self.heap_len] = node;
            self.tree(which)[node].fc = 1;
            self.depth[node] = 0;
            self.opt_len = self.opt_len.wrapping_sub(1);
            if has_static.is_some()
                && let Some(len) = self.static_len_of(which, node)
            {
                self.static_len = self.static_len.wrapping_sub(u64::from(len));
            }
        }
        match which {
            Which::Literals => self.l_max_code = max_code,
            Which::Distances => self.d_max_code = max_code,
            Which::BitLengths => self.bl_max_code = max_code,
        }
        // The elements heap[heap_len/2 + 1 .. heap_len] are leaves of the tree: a heap of
        // increasing frequency from the rest.
        for n in (1..=self.heap_len / 2).rev() {
            self.pqdownheap(which, n);
        }
        // The tree, by repeatedly combining the least two frequent nodes.
        let mut node = elems;
        loop {
            let n = self.heap[1];
            self.heap[1] = self.heap[self.heap_len];
            self.heap_len -= 1;
            self.pqdownheap(which, 1);
            let m = self.heap[1];
            self.heap_max -= 1;
            self.heap[self.heap_max] = n; // keep the nodes sorted by frequency
            self.heap_max -= 1;
            self.heap[self.heap_max] = m;
            let tree = self.tree(which);
            tree[node].fc = tree[n].fc.wrapping_add(tree[m].fc);
            let (dn, dm) = (self.depth[n], self.depth[m]);
            self.depth[node] = dn.max(dm).wrapping_add(1);
            let tree = self.tree(which);
            tree[n].dl = node as u16;
            tree[m].dl = node as u16;
            self.heap[1] = node;
            node += 1;
            self.pqdownheap(which, 1);
            if self.heap_len < 2 {
                break;
            }
        }
        self.heap_max -= 1;
        self.heap[self.heap_max] = self.heap[1];
        // The frequencies and parents are set: the optimal lengths, then the codes.
        self.gen_bitlen(which, max_code);
        let bl_count = self.bl_count;
        gen_codes(self.tree(which), max_code, &bl_count);
    }

    // The frequencies of the codes a tree's lengths are sent with.
    fn scan_tree(&mut self, which: Which, max_code: isize) {
        let mut prevlen: i32 = -1;
        let mut nextlen = i32::from(self.tree(which)[0].dl);
        let mut count = 0;
        let (mut max_count, mut min_count) = (7, 4);
        if nextlen == 0 {
            max_count = 138;
            min_count = 3;
        }
        self.tree(which)[(max_code + 1) as usize].dl = 0xffff; // guard
        for n in 0..=max_code.max(-1) {
            let n = n as usize;
            let curlen = nextlen;
            nextlen = i32::from(self.tree(which)[n + 1].dl);
            count += 1;
            if count < max_count && curlen == nextlen {
                continue;
            } else if count < min_count {
                let c = &mut self.bl_tree[curlen as usize].fc;
                *c = c.wrapping_add(count as u16);
            } else if curlen != 0 {
                if curlen != prevlen {
                    self.bl_tree[curlen as usize].fc += 1;
                }
                self.bl_tree[REP_3_6].fc += 1;
            } else if count <= 10 {
                self.bl_tree[REPZ_3_10].fc += 1;
            } else {
                self.bl_tree[REPZ_11_138].fc += 1;
            }
            count = 0;
            prevlen = curlen;
            if nextlen == 0 {
                max_count = 138;
                min_count = 3;
            } else if curlen == nextlen {
                max_count = 6;
                min_count = 3;
            } else {
                max_count = 7;
                min_count = 4;
            }
        }
    }

    fn send_code(&mut self, c: usize, tree: Which) {
        let t = self.tree(tree)[c];
        self.send_bits(u32::from(t.fc), u32::from(t.dl));
    }

    // A tree's lengths, in the bit length codes.
    fn send_tree(&mut self, which: Which, max_code: isize) {
        let mut prevlen: i32 = -1;
        let mut nextlen = i32::from(self.tree(which)[0].dl);
        let mut count = 0;
        let (mut max_count, mut min_count) = (7, 4);
        if nextlen == 0 {
            max_count = 138;
            min_count = 3;
        }
        for n in 0..=max_code.max(-1) {
            let n = n as usize;
            let curlen = nextlen;
            nextlen = i32::from(self.tree(which)[n + 1].dl);
            count += 1;
            if count < max_count && curlen == nextlen {
                continue;
            } else if count < min_count {
                loop {
                    self.send_code(curlen as usize, Which::BitLengths);
                    count -= 1;
                    if count == 0 {
                        break;
                    }
                }
            } else if curlen != 0 {
                if curlen != prevlen {
                    self.send_code(curlen as usize, Which::BitLengths);
                    count -= 1;
                }
                self.send_code(REP_3_6, Which::BitLengths);
                self.send_bits((count - 3) as u32, 2);
            } else if count <= 10 {
                self.send_code(REPZ_3_10, Which::BitLengths);
                self.send_bits((count - 3) as u32, 3);
            } else {
                self.send_code(REPZ_11_138, Which::BitLengths);
                self.send_bits((count - 11) as u32, 7);
            }
            count = 0;
            prevlen = curlen;
            if nextlen == 0 {
                max_count = 138;
                min_count = 3;
            } else if curlen == nextlen {
                max_count = 6;
                min_count = 3;
            } else {
                max_count = 7;
                min_count = 4;
            }
        }
    }

    // The bit length tree, and the index of the last bit length code to send.
    fn build_bl_tree(&mut self) -> usize {
        self.scan_tree(Which::Literals, self.l_max_code);
        self.scan_tree(Which::Distances, self.d_max_code);
        self.build_tree(Which::BitLengths);
        let mut max_blindex = BL_CODES - 1;
        while max_blindex >= 3 {
            if self.bl_tree[BL_ORDER[max_blindex]].dl != 0 {
                break;
            }
            max_blindex -= 1;
        }
        self.opt_len = self.opt_len.wrapping_add(3 * (max_blindex as u64 + 1) + 5 + 5 + 4);
        max_blindex
    }

    fn send_all_trees(&mut self, lcodes: usize, dcodes: usize, blcodes: usize) {
        self.send_bits((lcodes - 257) as u32, 5);
        self.send_bits((dcodes - 1) as u32, 5);
        self.send_bits((blcodes - 4) as u32, 4);
        for &order in &BL_ORDER[..blcodes] {
            let len = self.bl_tree[order].dl;
            self.send_bits(u32::from(len), 3);
        }
        self.send_tree(Which::Literals, lcodes as isize - 1);
        self.send_tree(Which::Distances, dcodes as isize - 1);
    }

    fn stored_block(&mut self, start: usize, stored_len: usize, last: bool) {
        self.send_bits(u32::from(last), 3); // STORED_BLOCK << 1
        self.bi_windup();
        self.put_short(stored_len as u16);
        self.put_short(!(stored_len as u16));
        let data = self.window[start..start + stored_len].to_vec();
        self.out.extend_from_slice(&data);
    }

    // The symbols of the block, in the codes given (the fixed ones, or the block's).
    fn compress_block(&mut self, fixed: bool) {
        let (lt, dt) = if fixed { (None, None) } else { (Some(Which::Literals), Some(Which::Distances)) };
        let mut sx = 0;
        while sx < self.sym_next {
            let mut dist = usize::from(self.sym_buf[sx]) | usize::from(self.sym_buf[sx + 1]) << 8;
            let mut lc = usize::from(self.sym_buf[sx + 2]);
            sx += 3;
            if dist == 0 {
                self.send_lit(lc, lt); // a literal byte
            } else {
                let code = usize::from(self.st.length_code[lc]);
                self.send_lit(code + LITERALS + 1, lt); // the length code
                let extra = EXTRA_LBITS[code];
                if extra != 0 {
                    lc -= self.st.base_length[code] as usize;
                    self.send_bits(lc as u32, extra); // the extra length bits
                }
                dist -= 1; // the match distance - 1
                let code = self.st.d_code(dist);
                self.send_dist(code, dt);
                let extra = EXTRA_DBITS[code];
                if extra != 0 {
                    dist -= self.st.base_dist[code] as usize;
                    self.send_bits(dist as u32, extra); // the extra distance bits
                }
            }
        }
        self.send_lit(END_BLOCK, lt);
    }

    fn send_lit(&mut self, c: usize, tree: Option<Which>) {
        let t = match tree {
            Some(w) => self.tree(w)[c],
            None => self.st.ltree[c],
        };
        self.send_bits(u32::from(t.fc), u32::from(t.dl));
    }

    fn send_dist(&mut self, c: usize, tree: Option<Which>) {
        let t = match tree {
            Some(w) => self.tree(w)[c],
            None => self.st.dtree[c],
        };
        self.send_bits(u32::from(t.fc), u32::from(t.dl));
    }

    // The block so far, stored, or in the fixed codes or its own, whichever is
    // shortest: `start` is where its bytes are in the window when they still are.
    fn flush_block(&mut self, start: Option<usize>, stored_len: usize, last: bool) {
        self.build_tree(Which::Literals);
        self.build_tree(Which::Distances);
        let max_blindex = self.build_bl_tree();
        let mut opt_lenb = self.opt_len.wrapping_add(3 + 7) >> 3;
        let static_lenb = self.static_len.wrapping_add(3 + 7) >> 3;
        if static_lenb <= opt_lenb {
            opt_lenb = static_lenb;
        }
        if let Some(start) = start
            && stored_len as u64 + 4 <= opt_lenb
        {
            self.stored_block(start, stored_len, last);
        } else if static_lenb == opt_lenb {
            self.send_bits((1 << 1) + u32::from(last), 3); // STATIC_TREES
            self.compress_block(true);
        } else {
            self.send_bits((2 << 1) + u32::from(last), 3); // DYN_TREES
            self.send_all_trees((self.l_max_code + 1) as usize, (self.d_max_code + 1) as usize, max_blindex + 1);
            self.compress_block(false);
        }
        self.init_block();
        if last {
            self.bi_windup();
        }
    }

    fn flush_block_only(&mut self, last: bool) {
        let start = if self.block_start >= 0 { Some(self.block_start as usize) } else { None };
        let len = (self.strstart as i64 - self.block_start) as usize;
        self.flush_block(start, len, last);
        self.block_start = self.strstart as i64;
    }

    fn tally_lit(&mut self, c: u8) -> bool {
        self.sym_buf[self.sym_next] = 0;
        self.sym_buf[self.sym_next + 1] = 0;
        self.sym_buf[self.sym_next + 2] = c;
        self.sym_next += 3;
        self.dyn_ltree[usize::from(c)].fc += 1;
        self.sym_next == SYM_END
    }

    fn tally_dist(&mut self, distance: usize, length: usize) -> bool {
        let (len, dist) = (length as u8, distance as u16);
        self.sym_buf[self.sym_next] = dist as u8;
        self.sym_buf[self.sym_next + 1] = (dist >> 8) as u8;
        self.sym_buf[self.sym_next + 2] = len;
        self.sym_next += 3;
        let dist = usize::from(dist.wrapping_sub(1));
        self.dyn_ltree[usize::from(self.st.length_code[usize::from(len)]) + LITERALS + 1].fc += 1;
        let d = self.st.d_code(dist);
        self.dyn_dtree[d].fc += 1;
        self.sym_next == SYM_END
    }

    // ---- matching -------------------------------------------------------------------------

    fn update_hash(&mut self, c: u8) {
        self.ins_h = ((self.ins_h << HASH_SHIFT) ^ u32::from(c)) & HASH_MASK;
    }

    // Inserts the string at `s` in the dictionary; the previous head of its hash chain.
    fn insert_string(&mut self, s: usize) -> usize {
        let c = self.window[s + MIN_MATCH - 1];
        self.update_hash(c);
        let h = self.ins_h as usize;
        let head = self.head[h];
        self.prev[s & W_MASK] = head;
        self.head[h] = s as u16;
        usize::from(head)
    }

    fn slide_hash(&mut self) {
        for p in self.head.iter_mut().chain(self.prev.iter_mut()) {
            let m = usize::from(*p);
            *p = if m >= W_SIZE { (m - W_SIZE) as u16 } else { 0 };
        }
    }

    // Fills the window when the lookahead becomes insufficient, sliding it when it is
    // almost full; `input` and `at`: what is left to read.
    fn fill_window(&mut self, input: &[u8], at: &mut usize) {
        loop {
            let mut more = WINDOW_SIZE - self.lookahead - self.strstart;
            if self.strstart >= W_SIZE + MAX_DIST {
                self.window.copy_within(W_SIZE..W_SIZE + W_SIZE - more, 0);
                self.match_start = self.match_start.wrapping_sub(W_SIZE);
                self.strstart -= W_SIZE; // now strstart >= MAX_DIST
                self.block_start -= W_SIZE as i64;
                if self.insert > self.strstart {
                    self.insert = self.strstart;
                }
                self.slide_hash();
                more += W_SIZE;
            }
            if *at == input.len() {
                break;
            }
            let n = (input.len() - *at).min(more);
            let to = self.strstart + self.lookahead;
            self.window[to..to + n].copy_from_slice(&input[*at..*at + n]);
            *at += n;
            self.lookahead += n;
            // The hash value, now that there is some input.
            if self.lookahead + self.insert >= MIN_MATCH {
                let mut s = self.strstart - self.insert;
                self.ins_h = u32::from(self.window[s]);
                let c = self.window[s + 1];
                self.update_hash(c);
                while self.insert != 0 {
                    let c = self.window[s + MIN_MATCH - 1];
                    self.update_hash(c);
                    self.prev[s & W_MASK] = self.head[self.ins_h as usize];
                    self.head[self.ins_h as usize] = s as u16;
                    s += 1;
                    self.insert -= 1;
                    if self.lookahead + self.insert < MIN_MATCH {
                        break;
                    }
                }
            }
            if !(self.lookahead < MIN_LOOKAHEAD && *at != input.len()) {
                break;
            }
        }
        // The WIN_INIT bytes after the end of the data that have never been written are
        // zeroed, for the matcher's reads past it.
        if self.high_water < WINDOW_SIZE {
            let curr = self.strstart + self.lookahead;
            if self.high_water < curr {
                let init = (WINDOW_SIZE - curr).min(WIN_INIT);
                self.window[curr..curr + init].fill(0);
                self.high_water = curr + init;
            } else if self.high_water < curr + WIN_INIT {
                let init = (curr + WIN_INIT - self.high_water).min(WINDOW_SIZE - self.high_water);
                let hw = self.high_water;
                self.window[hw..hw + init].fill(0);
                self.high_water += init;
            }
        }
    }

    // The longest match at strstart along the hash chain from `cur_match`, longer than
    // prev_length, no longer than the lookahead; match_start set to it.
    fn longest_match(&mut self, mut cur_match: usize) -> usize {
        let mut chain_length = self.max_chain_length;
        let scan = self.strstart;
        let mut best_len = self.prev_length;
        let mut nice_match = self.nice_match;
        let limit = self.strstart.saturating_sub(MAX_DIST);
        let w = &self.window;
        let strend = scan + MAX_MATCH;
        let mut scan_end1 = w[scan + best_len - 1];
        let mut scan_end = w[scan + best_len];
        if self.prev_length >= self.good_match {
            chain_length >>= 2;
        }
        if nice_match > self.lookahead {
            nice_match = self.lookahead;
        }
        loop {
            let m = cur_match;
            if w[m + best_len] == scan_end && w[m + best_len - 1] == scan_end1 && w[m] == w[scan] && w[m + 1] == w[scan + 1]
            {
                // scan[2] and match[2] are equal when the others are, the hash keys being
                // equal.
                let (mut s, mut mm) = (scan + 2, m + 2);
                loop {
                    let mut k = 0;
                    let mut same = true;
                    while k < 8 {
                        s += 1;
                        mm += 1;
                        if w[s] != w[mm] {
                            same = false;
                            break;
                        }
                        k += 1;
                    }
                    if !same || s >= strend {
                        break;
                    }
                }
                let len = MAX_MATCH - (strend - s);
                if len > best_len {
                    self.match_start = cur_match;
                    best_len = len;
                    if len >= nice_match {
                        break;
                    }
                    scan_end1 = w[scan + best_len - 1];
                    scan_end = w[scan + best_len];
                }
            }
            cur_match = usize::from(self.prev[cur_match & W_MASK]);
            if cur_match <= limit {
                break;
            }
            chain_length -= 1;
            if chain_length == 0 {
                break;
            }
        }
        best_len.min(self.lookahead)
    }

    // The input with lazy matching: a match is taken only if the next position has none
    // longer.
    fn deflate_slow(&mut self, input: &[u8]) {
        let mut at = 0;
        loop {
            // Enough lookahead, except at the end of the input: MAX_MATCH bytes for the
            // next match, plus MIN_MATCH bytes to insert the string following it.
            if self.lookahead < MIN_LOOKAHEAD {
                self.fill_window(input, &mut at);
                if self.lookahead == 0 {
                    break; // flush the current block
                }
            }
            let mut hash_head = 0;
            if self.lookahead >= MIN_MATCH {
                hash_head = self.insert_string(self.strstart);
            }
            // The longest match, discarding those <= prev_length.
            self.prev_length = self.match_length;
            self.prev_match = self.match_start;
            self.match_length = MIN_MATCH - 1;
            if hash_head != 0 && self.prev_length < self.max_lazy_match && self.strstart - hash_head <= MAX_DIST {
                // (Matches with the string of window index 0 are never taken.)
                self.match_length = self.longest_match(hash_head);
                if self.match_length <= 5
                    && self.match_length == MIN_MATCH
                    && self.strstart - self.match_start > TOO_FAR
                {
                    self.match_length = MIN_MATCH - 1;
                }
            }
            // A match at the previous step that the current one is no better than: it is
            // output.
            if self.prev_length >= MIN_MATCH && self.match_length <= self.prev_length {
                let max_insert = self.strstart + self.lookahead - MIN_MATCH;
                let bflush =
                    self.tally_dist(self.strstart - 1 - self.prev_match, self.prev_length - MIN_MATCH);
                // Every string up to the end of the match in the hash table; strstart - 1
                // and strstart are in already; the last two are not, without lookahead
                // enough.
                self.lookahead -= self.prev_length - 1;
                self.prev_length -= 2;
                loop {
                    self.strstart += 1;
                    if self.strstart <= max_insert {
                        self.insert_string(self.strstart);
                    }
                    self.prev_length -= 1;
                    if self.prev_length == 0 {
                        break;
                    }
                }
                self.match_available = false;
                self.match_length = MIN_MATCH - 1;
                self.strstart += 1;
                if bflush {
                    self.flush_block_only(false);
                }
            } else if self.match_available {
                // No match at the previous position, or one shorter than the current one:
                // a single literal.
                let c = self.window[self.strstart - 1];
                if self.tally_lit(c) {
                    self.flush_block_only(false);
                }
                self.strstart += 1;
                self.lookahead -= 1;
            } else {
                // No previous match to compare with: the next step decides.
                self.match_available = true;
                self.strstart += 1;
                self.lookahead -= 1;
            }
        }
        if self.match_available {
            let c = self.window[self.strstart - 1];
            self.tally_lit(c);
            self.match_available = false;
        }
        self.insert = self.strstart.min(MIN_MATCH - 1);
        self.flush_block_only(true);
    }
}

fn adler32(data: &[u8]) -> u32 {
    const BASE: u32 = 65521;
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += u32::from(x);
            b += a;
        }
        a %= BASE;
        b %= BASE;
    }
    b << 16 | a
}

/// `data` as a zlib stream, at `level`, 4 to 9 (6 is zlib's default).
pub fn zlib(data: &[u8], level: u32) -> Vec<u8> {
    // (good, lazy, nice, chain) of each level with lazy matching.
    const CONFIG: [(usize, usize, usize, u32); 6] =
        [(4, 4, 16, 16), (8, 16, 32, 32), (8, 16, 128, 128), (8, 32, 128, 256), (32, 128, 258, 1024), (32, 258, 258, 4096)];
    let level = level.clamp(4, 9);
    let (good, lazy, nice, chain) = CONFIG[level as usize - 4];
    let mut s = State {
        st: Static::new(),
        out: Vec::with_capacity(data.len() / 2 + 64),
        bi_buf: 0,
        bi_valid: 0,
        window: vec![0; WINDOW_SIZE],
        prev: vec![0; W_SIZE],
        head: vec![0; HASH_SIZE],
        ins_h: 0,
        high_water: 0,
        block_start: 0,
        strstart: 0,
        match_start: 0,
        lookahead: 0,
        prev_length: MIN_MATCH - 1,
        match_length: MIN_MATCH - 1,
        prev_match: 0,
        match_available: false,
        insert: 0,
        max_chain_length: chain,
        max_lazy_match: lazy,
        good_match: good,
        nice_match: nice,
        dyn_ltree: [Ct::default(); HEAP_SIZE],
        dyn_dtree: [Ct::default(); 2 * D_CODES + 1],
        bl_tree: [Ct::default(); 2 * BL_CODES + 1],
        l_max_code: 0,
        d_max_code: 0,
        bl_max_code: 0,
        bl_count: [0; MAX_BITS + 1],
        heap: [0; HEAP_SIZE],
        heap_len: 0,
        heap_max: 0,
        depth: [0; HEAP_SIZE],
        sym_buf: vec![0; LIT_BUFSIZE * 3],
        sym_next: 0,
        opt_len: 0,
        static_len: 0,
    };
    s.init_block();
    // The header: deflate with a 32 KB window, the level's flags, no dictionary, and the
    // check that makes it a multiple of 31.
    let flags: u32 = match level {
        0..=5 => 1,
        6 => 2,
        _ => 3,
    };
    let mut header = (8 + ((W_BITS - 8) << 4)) << 8 | flags << 6;
    header += 31 - header % 31;
    s.out.push((header >> 8) as u8);
    s.out.push(header as u8);
    s.deflate_slow(data);
    s.out.extend_from_slice(&adler32(data).to_be_bytes());
    s.out
}
