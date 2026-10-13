//! NEON and SVE2 kernels. NEON is written with intrinsics. SVE2 has none on
//! stable Rust, so those loops are assembly: each walks the column under a
//! `whilelo` predicate, which also covers the last, partial vector, except the
//! sorted intersection, which hands the rest of its lists to the plain merge.

use std::arch::aarch64::*;
use std::arch::asm;

use super::{HASH_MUL, Kernels, scalar};

pub(super) static NEON: Kernels = Kernels {
    name: "neon",
    select_eq: neon::select_eq,
    find_same: neon::find_same,
    select_moved: neon::select_moved,
    hash_mix: neon::hash_mix,
    hash_finish: neon::hash_finish,
    intersect_sorted: neon::intersect_sorted,
    and_words: neon::and_words,
};

pub(super) static SVE2: Kernels = Kernels {
    name: "sve2",
    select_eq: sve2::select_eq,
    find_same: sve2::find_same,
    select_moved: sve2::select_moved,
    hash_mix: sve2::hash_mix,
    hash_finish: sve2::hash_finish,
    intersect_sorted: sve2::intersect_sorted,
    and_words: sve2::and_words,
};

mod neon {
    use super::*;

    const LANES: usize = 4;

    pub(super) unsafe fn select_eq(
        column: *const u32,
        len: usize,
        value: u32,
        out: *mut u32,
    ) -> usize {
        let (mut row, mut found) = (0, 0);
        unsafe {
            let wanted = vdupq_n_u32(value);
            // Four vectors a step, with one test for the lot; hits are rare,
            // so a step that has one is walked cell by cell.
            while row + 4 * LANES <= len {
                let any = (0..4).fold(vdupq_n_u32(0), |any, at| {
                    let cells = vld1q_u32(column.add(row + at * LANES));
                    vorrq_u32(any, vceqq_u32(cells, wanted))
                });
                if vmaxvq_u32(any) != 0 {
                    found = scalar::select_eq_in(column, row..row + 4 * LANES, value, out, found);
                }
                row += 4 * LANES;
            }
            scalar::select_eq_in(column, row..len, value, out, found)
        }
    }

    pub(super) unsafe fn find_same(a: *const u32, b: *const u32, len: usize) -> usize {
        let mut row = 0;
        unsafe {
            while row + LANES <= len {
                let equal = vceqq_u32(vld1q_u32(a.add(row)), vld1q_u32(b.add(row)));
                if vmaxvq_u32(equal) != 0 {
                    break;
                }
                row += LANES;
            }
            row + scalar::find_same(a.add(row), b.add(row), len - row)
        }
    }

    pub(super) unsafe fn select_moved(
        column: *const u32,
        map: *const u32,
        map_len: usize,
        len: usize,
        out: *mut u32,
    ) -> usize {
        let (mut row, mut found) = (0, 0);
        unsafe {
            while row + LANES <= len {
                let cells = vld1q_u32(column.add(row));
                // NEON has no gather: four loads, one vector compare. A cell
                // outside the map reads as its complement, which differs.
                let read = |cell: u32| match (cell as usize) < map_len {
                    true => *map.add(cell as usize),
                    false => !cell,
                };
                let now = [
                    read(vgetq_lane_u32::<0>(cells)),
                    read(vgetq_lane_u32::<1>(cells)),
                    read(vgetq_lane_u32::<2>(cells)),
                    read(vgetq_lane_u32::<3>(cells)),
                ];
                if vminvq_u32(vceqq_u32(vld1q_u32(now.as_ptr()), cells)) == 0 {
                    let rows = row..row + LANES;
                    found = scalar::select_moved_in(column, map, map_len, rows, out, found);
                }
                row += LANES;
            }
            scalar::select_moved_in(column, map, map_len, row..len, out, found)
        }
    }

    pub(super) unsafe fn hash_mix(hash: *mut u32, column: *const u32, len: usize) {
        let mut row = 0;
        unsafe {
            let mul = vdupq_n_u32(HASH_MUL);
            while row + LANES <= len {
                let mixed = veorq_u32(vld1q_u32(hash.add(row)), vld1q_u32(column.add(row)));
                vst1q_u32(hash.add(row), vmulq_u32(mixed, mul));
                row += LANES;
            }
            scalar::hash_mix(hash.add(row), column.add(row), len - row);
        }
    }

    pub(super) unsafe fn hash_finish(hash: *mut u32, len: usize) {
        let mut row = 0;
        unsafe {
            while row + LANES <= len {
                let value = vld1q_u32(hash.add(row));
                vst1q_u32(hash.add(row), veorq_u32(value, vshrq_n_u32::<15>(value)));
                row += LANES;
            }
            scalar::hash_finish(hash.add(row), len - row);
        }
    }

    // The block compare of `x86::avx2::intersect_sorted`, with `b` rotated
    // through the four lanes by `ext`.
    pub(super) unsafe fn intersect_sorted(
        a: *const u32,
        alen: usize,
        b: *const u32,
        blen: usize,
        out: *mut u32,
    ) -> usize {
        let room = alen.min(blen);
        let (mut i, mut j, mut found) = (0, 0, 0);
        unsafe {
            let bits = vld1q_u32([1, 2, 4, 8].as_ptr());
            while i + LANES <= alen && j + LANES <= blen && found + LANES <= room {
                let left = vld1q_u32(a.add(i));
                let right = vld1q_u32(b.add(j));
                let equal = vorrq_u32(
                    vorrq_u32(
                        vceqq_u32(left, right),
                        vceqq_u32(left, vextq_u32::<1>(right, right)),
                    ),
                    vorrq_u32(
                        vceqq_u32(left, vextq_u32::<2>(right, right)),
                        vceqq_u32(left, vextq_u32::<3>(right, right)),
                    ),
                );
                let mut hits = vaddvq_u32(vandq_u32(equal, bits));
                while hits != 0 {
                    *out.add(found) = *a.add(i + hits.trailing_zeros() as usize);
                    found += 1;
                    hits &= hits - 1;
                }
                let (high_a, high_b) = (*a.add(i + LANES - 1), *b.add(j + LANES - 1));
                i += if high_a <= high_b { LANES } else { 0 };
                j += if high_b <= high_a { LANES } else { 0 };
            }
            found
                + scalar::intersect_sorted_within(
                    a.add(i),
                    alen - i,
                    b.add(j),
                    blen - j,
                    out.add(found),
                    room - found,
                )
        }
    }

    pub(super) unsafe fn and_words(a: *mut u64, b: *const u64, len: usize) {
        let mut word = 0;
        unsafe {
            while word + 2 <= len {
                vst1q_u64(
                    a.add(word),
                    vandq_u64(vld1q_u64(a.add(word)), vld1q_u64(b.add(word))),
                );
                word += 2;
            }
            scalar::and_words(a.add(word), b.add(word), len - word);
        }
    }
}

mod sve2 {
    use super::*;

    // In every loop `{i}` is the row, p0 the lanes of this vector that are
    // below `len`. `whilelo` sets the flags: `b.eq` when no lane is active,
    // `b.mi` when the first one is.

    #[target_feature(enable = "sve2")]
    pub(super) unsafe fn select_eq(
        column: *const u32,
        len: usize,
        value: u32,
        out: *mut u32,
    ) -> usize {
        let found: usize;
        unsafe {
            asm!(
                "mov {i}, #0",
                "mov {k}, #0",
                "dup z1.s, {v:w}",
                // z3 holds each lane's row.
                "index z3.s, #0, #1",
                "whilelo p0.s, {i}, {n}",
                "b.eq 3f",
                "2:",
                "ld1w z0.s, p0/z, [{p}, {i}, lsl #2]",
                "cmpeq p1.s, p0/z, z0.s, z1.s",
                "b.eq 4f",
                // Pack the rows of the hits to the front and store that many.
                "compact z2.s, p1, z3.s",
                "cntp {t}, p0, p1.s",
                "whilelo p2.s, xzr, {t}",
                "st1w z2.s, p2, [{o}, {k}, lsl #2]",
                "add {k}, {k}, {t}",
                "4:",
                "incw {i}",
                "incw z3.s",
                "whilelo p0.s, {i}, {n}",
                "b.mi 2b",
                "3:",
                p = in(reg) column,
                n = in(reg) len,
                v = in(reg) value,
                o = in(reg) out,
                i = out(reg) _,
                k = out(reg) found,
                t = out(reg) _,
                out("v0") _, out("v1") _, out("v2") _, out("v3") _,
                out("p0") _, out("p1") _, out("p2") _,
                options(nostack),
            );
        }
        found
    }

    #[target_feature(enable = "sve2")]
    pub(super) unsafe fn find_same(a: *const u32, b: *const u32, len: usize) -> usize {
        let row: usize;
        unsafe {
            asm!(
                "mov {i}, #0",
                "whilelo p0.s, {i}, {n}",
                "b.eq 3f",
                "2:",
                "ld1w z0.s, p0/z, [{a}, {i}, lsl #2]",
                "ld1w z1.s, p0/z, [{b}, {i}, lsl #2]",
                "cmpeq p1.s, p0/z, z0.s, z1.s",
                "b.ne 4f",
                "incw {i}",
                "whilelo p0.s, {i}, {n}",
                "b.mi 2b",
                "3:",
                "mov {i}, {n}",
                "b 5f",
                "4:",
                "brkb p1.b, p0/z, p1.b",
                "incp {i}, p1.s",
                "5:",
                a = in(reg) a,
                b = in(reg) b,
                n = in(reg) len,
                i = out(reg) row,
                out("v0") _, out("v1") _, out("p0") _, out("p1") _,
                options(nostack, readonly),
            );
        }
        row
    }

    #[target_feature(enable = "sve2")]
    pub(super) unsafe fn select_moved(
        column: *const u32,
        map: *const u32,
        map_len: usize,
        len: usize,
        out: *mut u32,
    ) -> usize {
        let found: usize;
        unsafe {
            asm!(
                "mov {i}, #0",
                "mov {k}, #0",
                "dup z2.s, {l:w}",
                // z3 holds each lane's row.
                "index z3.s, #0, #1",
                "whilelo p0.s, {i}, {n}",
                "b.eq 3f",
                "2:",
                "ld1w z0.s, p0/z, [{c}, {i}, lsl #2]",
                // p2: lanes outside the map; p3: lanes inside it, gathered.
                "cmphs p2.s, p0/z, z0.s, z2.s",
                "cmphi p3.s, p0/z, z2.s, z0.s",
                "ld1w z1.s, p3/z, [{m}, z0.s, uxtw #2]",
                "cmpne p1.s, p3/z, z1.s, z0.s",
                "orrs p1.b, p0/z, p1.b, p2.b",
                "b.eq 4f",
                // Pack the rows of the hits to the front and store that many.
                "compact z4.s, p1, z3.s",
                "cntp {t}, p0, p1.s",
                "whilelo p2.s, xzr, {t}",
                "st1w z4.s, p2, [{o}, {k}, lsl #2]",
                "add {k}, {k}, {t}",
                "4:",
                "incw {i}",
                "incw z3.s",
                "whilelo p0.s, {i}, {n}",
                "b.mi 2b",
                "3:",
                c = in(reg) column,
                m = in(reg) map,
                l = in(reg) map_len,
                n = in(reg) len,
                o = in(reg) out,
                i = out(reg) _,
                k = out(reg) found,
                t = out(reg) _,
                out("v0") _, out("v1") _, out("v2") _, out("v3") _, out("v4") _,
                out("p0") _, out("p1") _, out("p2") _, out("p3") _,
                options(nostack),
            );
        }
        found
    }

    #[target_feature(enable = "sve2")]
    pub(super) unsafe fn hash_mix(hash: *mut u32, column: *const u32, len: usize) {
        unsafe {
            asm!(
                "mov {i}, #0",
                "dup z2.s, {k:w}",
                "whilelo p0.s, {i}, {n}",
                "b.eq 3f",
                "2:",
                "ld1w z0.s, p0/z, [{h}, {i}, lsl #2]",
                "ld1w z1.s, p0/z, [{c}, {i}, lsl #2]",
                "eor z0.d, z0.d, z1.d",
                "mul z0.s, p0/m, z0.s, z2.s",
                "st1w z0.s, p0, [{h}, {i}, lsl #2]",
                "incw {i}",
                "whilelo p0.s, {i}, {n}",
                "b.mi 2b",
                "3:",
                h = in(reg) hash,
                c = in(reg) column,
                n = in(reg) len,
                k = in(reg) HASH_MUL,
                i = out(reg) _,
                out("v0") _, out("v1") _, out("v2") _, out("p0") _,
                options(nostack),
            );
        }
    }

    #[target_feature(enable = "sve2")]
    pub(super) unsafe fn hash_finish(hash: *mut u32, len: usize) {
        unsafe {
            asm!(
                "mov {i}, #0",
                "whilelo p0.s, {i}, {n}",
                "b.eq 3f",
                "2:",
                "ld1w z0.s, p0/z, [{h}, {i}, lsl #2]",
                "lsr z1.s, z0.s, #15",
                "eor z0.d, z0.d, z1.d",
                "st1w z0.s, p0, [{h}, {i}, lsl #2]",
                "incw {i}",
                "whilelo p0.s, {i}, {n}",
                "b.mi 2b",
                "3:",
                h = in(reg) hash,
                n = in(reg) len,
                i = out(reg) _,
                out("v0") _, out("v1") _, out("p0") _,
                options(nostack),
            );
        }
    }

    // The block compare of `x86::avx2::intersect_sorted` over whole vectors,
    // `{w}` lanes each: `ext` rotates `b` a lane at a time under `a`, and
    // `compact` packs the lanes of `a` that met an equal lane. SVE2's `match`
    // would do the all-pairs compare in one instruction, but it takes only
    // bytes and halfwords. `{i}`, `{j}` and `{k}` are the rows of `a`, `b` and
    // `out` reached; `{room}` is what `out` holds.
    #[target_feature(enable = "sve2")]
    pub(super) unsafe fn intersect_sorted(
        a: *const u32,
        alen: usize,
        b: *const u32,
        blen: usize,
        out: *mut u32,
    ) -> usize {
        let room = alen.min(blen);
        let (i, j, found): (usize, usize, usize);
        unsafe {
            asm!(
                "mov {i}, #0",
                "mov {j}, #0",
                "mov {k}, #0",
                "cntw {w}",
                "ptrue p0.s",
                "2:",
                "add {t}, {i}, {w}",
                "cmp {t}, {alen}",
                "b.hi 4f",
                "add {t}, {j}, {w}",
                "cmp {t}, {blen}",
                "b.hi 4f",
                "add {t}, {k}, {w}",
                "cmp {t}, {room}",
                "b.hi 4f",
                "ld1w z0.s, p0/z, [{a}, {i}, lsl #2]",
                "ld1w z1.s, p0/z, [{b}, {j}, lsl #2]",
                "pfalse p1.b",
                "mov {t}, {w}",
                "3:",
                "cmpeq p2.s, p0/z, z0.s, z1.s",
                "orr p1.b, p0/z, p1.b, p2.b",
                "ext z1.b, z1.b, z1.b, #4",
                "subs {t}, {t}, #1",
                "b.ne 3b",
                "compact z2.s, p1, z0.s",
                "cntp {t}, p0, p1.s",
                "whilelo p2.s, xzr, {t}",
                "st1w z2.s, p2, [{o}, {k}, lsl #2]",
                "add {k}, {k}, {t}",
                // After a full turn `b` is back in place; step past the
                // vector that ends lower, or both on a tie.
                "lastb {x:w}, p0, z0.s",
                "lastb {y:w}, p0, z1.s",
                "cmp {x:w}, {y:w}",
                "csel {t}, {w}, xzr, ls",
                "add {i}, {i}, {t}",
                "csel {t}, {w}, xzr, hs",
                "add {j}, {j}, {t}",
                "b 2b",
                "4:",
                a = in(reg) a,
                b = in(reg) b,
                o = in(reg) out,
                alen = in(reg) alen,
                blen = in(reg) blen,
                room = in(reg) room,
                i = out(reg) i,
                j = out(reg) j,
                k = out(reg) found,
                w = out(reg) _,
                t = out(reg) _,
                x = out(reg) _,
                y = out(reg) _,
                out("v0") _, out("v1") _, out("v2") _, out("p0") _, out("p1") _, out("p2") _,
                options(nostack),
            );
            found
                + scalar::intersect_sorted_within(
                    a.add(i),
                    alen - i,
                    b.add(j),
                    blen - j,
                    out.add(found),
                    room - found,
                )
        }
    }

    #[target_feature(enable = "sve2")]
    pub(super) unsafe fn and_words(a: *mut u64, b: *const u64, len: usize) {
        unsafe {
            asm!(
                "mov {i}, #0",
                "whilelo p0.d, {i}, {n}",
                "b.eq 3f",
                "2:",
                "ld1d z0.d, p0/z, [{a}, {i}, lsl #3]",
                "ld1d z1.d, p0/z, [{b}, {i}, lsl #3]",
                "and z0.d, z0.d, z1.d",
                "st1d z0.d, p0, [{a}, {i}, lsl #3]",
                "incd {i}",
                "whilelo p0.d, {i}, {n}",
                "b.mi 2b",
                "3:",
                a = in(reg) a,
                b = in(reg) b,
                n = in(reg) len,
                i = out(reg) _,
                out("v0") _, out("v1") _, out("p0") _,
                options(nostack),
            );
        }
    }
}
