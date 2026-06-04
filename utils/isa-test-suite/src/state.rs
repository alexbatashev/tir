//! The architectural state we capture from each oracle and compare.
//!
//! Kept deliberately small: integer registers, the program counter, and any
//! requested memory windows. That is enough to catch the bulk of TMDL semantic
//! errors (wrong result, wrong destination, wrong address) while keeping both
//! oracles cheap to drive. FP/CSR/flag state is future work.

pub const GPR_COUNT: usize = 32;

/// A contiguous span of memory captured at the end of a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemWindow {
    pub addr: u64,
    pub bytes: Vec<u8>,
}

/// Final architectural state of a snippet, as observed by one oracle.
#[derive(Debug, Clone)]
pub struct ArchState {
    /// `gprs[i]` is integer register `x{i}` (`x0` is always 0).
    pub gprs: [u64; GPR_COUNT],
    pub pc: u64,
    pub mem: Vec<MemWindow>,
}

impl ArchState {
    /// Compare this state (produced by the simulator under test) against the
    /// `golden` reference. Returns one human-readable line per disagreement;
    /// an empty vector means the states match.
    pub fn diff(&self, golden: &ArchState) -> Vec<String> {
        let mut out = Vec::new();

        if self.pc != golden.pc {
            out.push(format!(
                "pc: isasim=0x{:x} golden=0x{:x}",
                self.pc, golden.pc
            ));
        }

        for i in 0..GPR_COUNT {
            if self.gprs[i] != golden.gprs[i] {
                out.push(format!(
                    "x{i}: isasim=0x{:x} golden=0x{:x}",
                    self.gprs[i], golden.gprs[i]
                ));
            }
        }

        // Memory windows are captured in the same order by both oracles, so we
        // line them up positionally and report the first differing byte of each.
        for (idx, (a, b)) in self.mem.iter().zip(golden.mem.iter()).enumerate() {
            if a.addr != b.addr {
                out.push(format!(
                    "mem window {idx}: isasim addr=0x{:x} golden addr=0x{:x}",
                    a.addr, b.addr
                ));
                continue;
            }
            if a.bytes != b.bytes {
                for (off, (x, y)) in a.bytes.iter().zip(b.bytes.iter()).enumerate() {
                    if x != y {
                        out.push(format!(
                            "mem[0x{:x}]: isasim=0x{x:02x} golden=0x{y:02x}",
                            a.addr + off as u64
                        ));
                    }
                }
                if a.bytes.len() != b.bytes.len() {
                    out.push(format!(
                        "mem window 0x{:x}: isasim len={} golden len={}",
                        a.addr,
                        a.bytes.len(),
                        b.bytes.len()
                    ));
                }
            }
        }
        if self.mem.len() != golden.mem.len() {
            out.push(format!(
                "memory window count: isasim={} golden={}",
                self.mem.len(),
                golden.mem.len()
            ));
        }

        out
    }
}
