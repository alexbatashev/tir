//! The kernels in plain Rust: the reference the others are tested against, and
//! what runs where no instruction set below applies.

use super::{HASH_MUL, Kernels};

pub(super) static SCALAR: Kernels = Kernels {
    name: "scalar",
    find_eq,
    find_same,
    max,
    mark_moved,
    hash_mix,
    hash_finish,
    intersect_sorted,
    and_words,
};

pub(super) unsafe fn find_eq(column: *const u32, len: usize, value: u32) -> usize {
    (0..len)
        .find(|&row| unsafe { *column.add(row) } == value)
        .unwrap_or(len)
}

pub(super) unsafe fn find_same(a: *const u32, b: *const u32, len: usize) -> usize {
    (0..len)
        .find(|&row| unsafe { *a.add(row) == *b.add(row) })
        .unwrap_or(len)
}

pub(super) unsafe fn max(column: *const u32, len: usize) -> u32 {
    (0..len)
        .map(|row| unsafe { *column.add(row) })
        .max()
        .unwrap_or(0)
}

pub(super) unsafe fn mark_moved(column: *const u32, map: *const u32, flags: *mut u32, len: usize) {
    for row in 0..len {
        unsafe {
            let cell = *column.add(row);
            *flags.add(row) |= u32::from(*map.add(cell as usize) != cell);
        }
    }
}

pub(super) unsafe fn hash_mix(hash: *mut u32, column: *const u32, len: usize) {
    for row in 0..len {
        unsafe { *hash.add(row) = (*hash.add(row) ^ *column.add(row)).wrapping_mul(HASH_MUL) };
    }
}

pub(super) unsafe fn hash_finish(hash: *mut u32, len: usize) {
    for row in 0..len {
        unsafe { *hash.add(row) ^= *hash.add(row) >> 15 };
    }
}

pub(super) unsafe fn intersect_sorted(
    a: *const u32,
    alen: usize,
    b: *const u32,
    blen: usize,
    out: *mut u32,
) -> usize {
    unsafe { intersect_sorted_within(a, alen, b, blen, out, alen.min(blen)) }
}

// The merge, writing at most `room` cells. A vector kernel hands it the rest of
// both lists with the room it has left, which keeps the kernel inside `out`
// even when the lists are not ascending.
pub(super) unsafe fn intersect_sorted_within(
    a: *const u32,
    alen: usize,
    b: *const u32,
    blen: usize,
    out: *mut u32,
    room: usize,
) -> usize {
    let (mut i, mut j, mut found) = (0, 0, 0);
    while i < alen && j < blen && found < room {
        let (x, y) = unsafe { (*a.add(i), *b.add(j)) };
        if x == y {
            unsafe { *out.add(found) = x };
            found += 1;
        }
        i += usize::from(x <= y);
        j += usize::from(y <= x);
    }
    found
}

pub(super) unsafe fn and_words(a: *mut u64, b: *const u64, len: usize) {
    for word in 0..len {
        unsafe { *a.add(word) &= *b.add(word) };
    }
}
