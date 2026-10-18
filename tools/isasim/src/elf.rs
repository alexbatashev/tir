//! Minimal loader for statically-linked ELF64 little-endian executables. Reads
//! the program headers (which the object-file reader in `tir` core discards) to
//! recover the entry point and `PT_LOAD` segments, so the simulator can place a
//! real program image into guest memory and run it by decode-on-fetch.

/// A `PT_LOAD` segment: `filesz` bytes copied from the file at `offset`. Any
/// `.bss` tail (memsz > filesz) needs no action — guest memory starts zeroed.
pub struct Segment {
    pub vaddr: u64,
    pub offset: u64,
    pub filesz: u64,
}

pub struct LoadedElf {
    pub entry: u64,
    pub segments: Vec<Segment>,
    /// Lowest loaded virtual address across all segments.
    pub min_vaddr: u64,
    /// Highest `vaddr + memsz` across all segments.
    pub max_vaddr_end: u64,
}

const PT_LOAD: u32 = 1;
const ET_EXEC: u16 = 2;
const ET_DYN: u16 = 3;

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes(b[o..o + 2].try_into().unwrap())
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

/// Parse an ELF64 LE executable's header and `PT_LOAD` segments.
pub fn load_executable(bytes: &[u8]) -> Result<LoadedElf, String> {
    if bytes.len() < 64 || &bytes[0..4] != b"\x7fELF" {
        return Err("not an ELF file".into());
    }
    if bytes[4] != 2 {
        return Err("only ELF64 is supported".into());
    }
    if bytes[5] != 1 {
        return Err("only little-endian ELF is supported".into());
    }
    let e_type = u16_at(bytes, 16);
    if e_type != ET_EXEC && e_type != ET_DYN {
        return Err(format!("unsupported ELF type {e_type} (need EXEC or DYN)"));
    }
    let entry = u64_at(bytes, 24);
    let phoff = u64_at(bytes, 32) as usize;
    let phentsize = u16_at(bytes, 54) as usize;
    let phnum = u16_at(bytes, 56) as usize;

    let mut segments = Vec::new();
    let mut min_vaddr = u64::MAX;
    let mut max_vaddr_end = 0u64;
    for i in 0..phnum {
        let ph = phoff + i * phentsize;
        if ph + 56 > bytes.len() {
            return Err("truncated program header".into());
        }
        if u32_at(bytes, ph) != PT_LOAD {
            continue;
        }
        let offset = u64_at(bytes, ph + 8);
        let vaddr = u64_at(bytes, ph + 16);
        let filesz = u64_at(bytes, ph + 32);
        let memsz = u64_at(bytes, ph + 40);
        if memsz == 0 {
            continue;
        }
        min_vaddr = min_vaddr.min(vaddr);
        max_vaddr_end = max_vaddr_end.max(vaddr + memsz);
        segments.push(Segment {
            vaddr,
            offset,
            filesz,
        });
    }
    if segments.is_empty() {
        return Err("no loadable segments".into());
    }
    Ok(LoadedElf {
        entry,
        segments,
        min_vaddr,
        max_vaddr_end,
    })
}
