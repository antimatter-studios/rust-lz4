//! The high-compression encoder: a hash chain finds matches, and either lazy
//! matching (levels 1 to 9) or a shortest-output search over a window of
//! positions (levels 10 to 12) chooses among them.

const MIN_MATCH: usize = 4;
/// The last five bytes of a block are literals.
const LAST_LITERALS: usize = 5;
/// The last match starts at least this far before the end of the block.
const MATCH_START_MARGIN: usize = 12;
const MAX_OFFSET: usize = 65535;
const HASH_BITS: u32 = 16;
const NO_POSITION: u32 = u32::MAX;
/// How many positions one shortest-output search covers.
const OPTIMAL_WINDOW: usize = 4096;

/// The most a block of `len` input bytes can compress to.
pub fn compress_bound(len: usize) -> usize {
    len + len / 255 + 16
}

/// How the shortest output found so far reaches a position.
#[derive(Clone, Copy)]
struct Reach {
    price: u64,
    literals: u32,
    length: u32,
    offset: u16,
}

/// One sequence: `literals` bytes from the input, then a match.
#[derive(Clone, Copy)]
struct Sequence {
    literals: usize,
    length: usize,
    offset: usize,
}

/// Extra bytes a count needs beyond its nibble.
fn extra_bytes(count: usize) -> usize {
    if count < 15 {
        0
    } else {
        (count - 15) / 255 + 1
    }
}

fn sequence_size(s: &Sequence) -> usize {
    1 + extra_bytes(s.literals) + s.literals + 2 + extra_bytes(s.length - MIN_MATCH)
}

fn final_literals_size(literals: usize) -> usize {
    1 + extra_bytes(literals) + literals
}

fn put_extra(dst: &mut [u8], at: &mut usize, count: usize) {
    if count >= 15 {
        let mut left = count - 15;
        while left >= 255 {
            dst[*at] = 255;
            *at += 1;
            left -= 255;
        }
        dst[*at] = left as u8;
        *at += 1;
    }
}

fn hash(bytes: &[u8]) -> usize {
    let v = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    (v.wrapping_mul(2_654_435_761) >> (32 - HASH_BITS)) as usize
}

fn common_prefix(src: &[u8], a: usize, b: usize, limit: usize) -> usize {
    let mut n = 0;
    while b + n < limit && src[a + n] == src[b + n] {
        n += 1;
    }
    n
}

/// An LZ4HC encoder, and the working tables it reuses across calls.
pub struct HcEncoder {
    level: u8,
    /// The latest position with each hash.
    head: Vec<u32>,
    /// For a position modulo the window, the previous one with its hash.
    previous: Vec<u32>,
    /// The next position to enter into the tables.
    next_insert: usize,
    /// The longest match found at each position already searched.
    found: Vec<(u32, u16)>,
}

impl HcEncoder {
    /// An encoder at `level`, clamped to 1..=12.
    pub fn new(level: u8) -> Self {
        Self {
            level: level.clamp(1, crate::MAX_LEVEL),
            head: vec![NO_POSITION; 1 << HASH_BITS],
            previous: vec![NO_POSITION; MAX_OFFSET + 1],
            next_insert: 0,
            found: Vec::new(),
        }
    }

    fn attempts(&self) -> usize {
        1 << (self.level as usize - 1).min(13)
    }

    fn reset(&mut self) {
        self.head.fill(NO_POSITION);
        self.previous.fill(NO_POSITION);
        self.next_insert = 0;
        self.found.clear();
    }

    /// The longest match at `pos`, ending before `limit`, from positions
    /// inserted so far. Results are kept, so a position searched again by a
    /// later window costs nothing.
    fn longest_match(&mut self, src: &[u8], pos: usize, limit: usize) -> (usize, usize) {
        if let Some(&(length, offset)) = self.found.get(pos) {
            if length != u32::MAX {
                return (length as usize, offset as usize);
            }
        }
        while self.next_insert < pos {
            let at = self.next_insert;
            let h = hash(&src[at..]);
            self.previous[at & MAX_OFFSET] = self.head[h];
            self.head[h] = at as u32;
            self.next_insert += 1;
        }
        let mut best = (0usize, 0usize);
        let mut candidate = self.head[hash(&src[pos..])];
        let mut left = self.attempts();
        while candidate != NO_POSITION && left > 0 {
            let at = candidate as usize;
            if pos - at > MAX_OFFSET {
                break;
            }
            let length = common_prefix(src, at, pos, limit);
            if length > best.0 {
                best = (length, pos - at);
            }
            let next = self.previous[at & MAX_OFFSET];
            if next != NO_POSITION && next as usize >= at {
                break;
            }
            candidate = next;
            left -= 1;
        }
        if best.0 < MIN_MATCH {
            best = (0, 0);
        }
        if self.found.len() <= pos {
            self.found.resize(pos + 1, (u32::MAX, 0));
        }
        self.found[pos] = (best.0 as u32, best.1 as u16);
        best
    }

    /// The sequences from `anchor` on, as far as one parsing step reaches,
    /// and where the next step starts. Empty when no match starts in reach.
    fn parse_step(
        &mut self,
        src: &[u8],
        anchor: usize,
        match_start_limit: usize,
        limit: usize,
    ) -> (Vec<Sequence>, usize) {
        if self.level >= 10 {
            self.parse_optimal(src, anchor, match_start_limit, limit)
        } else {
            self.parse_lazy(src, anchor, match_start_limit, limit)
        }
    }

    /// Take the next match, unless the position after it starts a longer one.
    fn parse_lazy(
        &mut self,
        src: &[u8],
        anchor: usize,
        match_start_limit: usize,
        limit: usize,
    ) -> (Vec<Sequence>, usize) {
        let mut pos = anchor;
        while pos < match_start_limit {
            let (length, offset) = self.longest_match(src, pos, limit);
            if length == 0 {
                pos += 1;
                continue;
            }
            if pos + 1 < match_start_limit {
                let (next_length, _) = self.longest_match(src, pos + 1, limit);
                if next_length > length {
                    pos += 1;
                    continue;
                }
            }
            let sequence = Sequence {
                literals: pos - anchor,
                length,
                offset,
            };
            return (vec![sequence], pos + length);
        }
        (Vec::new(), pos)
    }

    /// The shortest output over the next window of positions: each position
    /// is reached by a literal or by any length of a match found behind it.
    fn parse_optimal(
        &mut self,
        src: &[u8],
        anchor: usize,
        match_start_limit: usize,
        limit: usize,
    ) -> (Vec<Sequence>, usize) {
        let unreached = Reach {
            price: u64::MAX,
            literals: 0,
            length: 0,
            offset: 0,
        };
        let span = OPTIMAL_WINDOW.min(limit - anchor);
        let mut reach = vec![unreached; span + 1];
        reach[0].price = 0;
        for i in 0..span {
            let here = reach[i];
            if here.price == u64::MAX {
                continue;
            }
            let literals = here.literals as usize + 1;
            let extra = u64::from(literals >= 15 && (literals - 15).is_multiple_of(255));
            let price = here.price + 1 + extra;
            if price < reach[i + 1].price {
                reach[i + 1] = Reach {
                    price,
                    literals: literals as u32,
                    length: 0,
                    offset: 0,
                };
            }
            let pos = anchor + i;
            if pos >= match_start_limit {
                continue;
            }
            let (length, offset) = self.longest_match(src, pos, limit);
            if length == 0 {
                continue;
            }
            // A match running past the window is taken whole and ends it.
            if i + length > span {
                return self.finish_optimal(&reach, anchor, i, length, offset);
            }
            for take in MIN_MATCH..=length {
                let price = here.price + 3 + extra_bytes(take - MIN_MATCH) as u64;
                if price <= reach[i + take].price {
                    reach[i + take] = Reach {
                        price,
                        literals: 0,
                        length: take as u32,
                        offset: offset as u16,
                    };
                }
            }
        }
        // Back from the window's end, then keep what ends in a match.
        let mut steps = Vec::new();
        let mut i = span;
        while i > 0 {
            let r = reach[i];
            if r.length > 0 {
                steps.push((i - r.length as usize, r.length as usize, r.offset as usize));
                i -= r.length as usize;
            } else {
                i -= 1;
            }
        }
        steps.reverse();
        let mut sequences = Vec::with_capacity(steps.len());
        let mut at = anchor;
        for (start, length, offset) in steps {
            sequences.push(Sequence {
                literals: anchor + start - at,
                length,
                offset,
            });
            at = anchor + start + length;
        }
        if sequences.is_empty() {
            return (sequences, anchor + span);
        }
        (sequences, at)
    }

    /// The sequences reaching position `i`, then a match past the window.
    fn finish_optimal(
        &self,
        reach: &[Reach],
        anchor: usize,
        i: usize,
        length: usize,
        offset: usize,
    ) -> (Vec<Sequence>, usize) {
        let mut steps = vec![(i, length, offset)];
        let mut back = i;
        while back > 0 {
            let r = reach[back];
            if r.length > 0 {
                steps.push((
                    back - r.length as usize,
                    r.length as usize,
                    r.offset as usize,
                ));
                back -= r.length as usize;
            } else {
                back -= 1;
            }
        }
        steps.reverse();
        let mut sequences = Vec::with_capacity(steps.len());
        let mut at = anchor;
        for (start, length, offset) in steps {
            sequences.push(Sequence {
                literals: anchor + start - at,
                length,
                offset,
            });
            at = anchor + start + length;
        }
        (sequences, at)
    }

    /// Compress as much of `src` as fits in `dst`, as one block that decodes
    /// on its own. Returns the input bytes taken and the output bytes written;
    /// `(0, 0)` when `dst` is empty.
    pub fn compress_dest_size(&mut self, src: &[u8], dst: &mut [u8]) -> (usize, usize) {
        if dst.is_empty() {
            return (0, 0);
        }
        self.reset();
        let limit = src.len().saturating_sub(LAST_LITERALS);
        let match_start_limit = src.len().saturating_sub(MATCH_START_MARGIN);
        let (mut anchor, mut out) = (0usize, 0usize);
        // Where the parser has reached with no match yet: literals so far.
        let mut scan = 0usize;
        'parsing: while scan < match_start_limit {
            let (sequences, next) = self.parse_step(src, scan, match_start_limit, limit);
            if sequences.is_empty() {
                scan = next.max(scan + 1);
                continue;
            }
            let mut first = true;
            for s in sequences {
                let s = Sequence {
                    literals: if first {
                        s.literals + scan - anchor
                    } else {
                        s.literals
                    },
                    ..s
                };
                first = false;
                // Room for this sequence and for what must follow it: at
                // least five literals, and enough that its match starts
                // twelve bytes before the end.
                let tail = LAST_LITERALS.max(MATCH_START_MARGIN.saturating_sub(s.length));
                if out + sequence_size(&s) + final_literals_size(tail) > dst.len() {
                    break 'parsing;
                }
                out = put_sequence(src, dst, out, anchor, &s);
                anchor += s.literals + s.length;
            }
            scan = anchor;
        }
        let left = src.len() - anchor;
        let room = dst.len() - out;
        let mut literals = left.min(room.saturating_sub(1));
        while literals > 0 && final_literals_size(literals) > room {
            literals -= 1;
        }
        if room == 0 {
            return (anchor, out);
        }
        dst[out] = (literals.min(15) as u8) << 4;
        let mut at = out + 1;
        put_extra(dst, &mut at, literals);
        dst[at..at + literals].copy_from_slice(&src[anchor..anchor + literals]);
        (anchor + literals, at + literals)
    }
}

fn put_sequence(src: &[u8], dst: &mut [u8], out: usize, anchor: usize, s: &Sequence) -> usize {
    let match_nibble = s.length - MIN_MATCH;
    dst[out] = ((s.literals.min(15) as u8) << 4) | (match_nibble.min(15) as u8);
    let mut at = out + 1;
    put_extra(dst, &mut at, s.literals);
    dst[at..at + s.literals].copy_from_slice(&src[anchor..anchor + s.literals]);
    at += s.literals;
    dst[at..at + 2].copy_from_slice(&(s.offset as u16).to_le_bytes());
    at += 2;
    put_extra(dst, &mut at, match_nibble);
    at
}

/// Compress as much of `src` as fits in `dst`. See
/// [`HcEncoder::compress_dest_size`].
pub fn compress_hc_dest_size(src: &[u8], dst: &mut [u8], level: u8) -> (usize, usize) {
    HcEncoder::new(level).compress_dest_size(src, dst)
}

/// Compress all of `src` into one block.
pub fn compress_hc(src: &[u8], level: u8) -> Vec<u8> {
    let mut out = vec![0u8; compress_bound(src.len())];
    let (taken, written) = compress_hc_dest_size(src, &mut out, level);
    debug_assert_eq!(taken, src.len());
    out.truncate(written);
    out
}
