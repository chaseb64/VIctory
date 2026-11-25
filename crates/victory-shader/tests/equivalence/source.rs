//! Synthetic, single-dword GFX10 instructions. Targets are absolute source
//! instruction indices; only the encoder converts them to PC-relative words.
use super::{Inputs, LIMIT, Stop};

const EXEC: u8 = 126;
const VCC: u8 = 106;
const FIRST_SAVE: u8 = 3;
const MASK: u8 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Alu {
    Add(u8),    // v0 += inline constant
    XorData,    // v0 ^= v1
    AddData,    // v0 += v1
    UpdateData, // v1 += v0
    Set(u8),    // v0 = inline constant
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mask {
    Input,
    Compare(u8), // unsigned threshold < current v0
    Joined(u8),  // uniform choice between a compare and the input mask
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Region {
    Plain(Alu),
    If {
        yes: Alu,
        no: Alu,
    },
    Masked {
        mask: Mask,
        skip: bool,
        body: Vec<Region>,
    },
    AdvanceTo(u8), // while v0 < limit: v1 += v0, v0 += 1
    Repeat {
        times: u8,
        body: Vec<Region>,
    },
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Src {
    S(u8),
    Imm(u8),
}
use Src::{Imm, S};

#[derive(Clone, Copy, Debug)]
pub(super) enum Op {
    Mov(u8, Src),
    Inc(u8),
    Less(u8, u8),
    Vector(Alu),
    Compare(u8),
    Below(u8),
    Narrow { save: u8, mask: u8 },
    Scc1(usize),
    ExecZ(usize),
    Jump(usize),
    Observe,
    End,
}
use Op::*;

fn regions(body: &[Region], ops: &mut Vec<Op>, counter: u8, save: u8) {
    for region in body {
        match region {
            Region::Plain(a) => ops.push(Vector(*a)),
            Region::If { yes, no } => {
                let b = ops.len();
                ops.extend([
                    Less(1, 1),
                    Scc1(b + 4),
                    Vector(*no),
                    Jump(b + 5),
                    Vector(*yes),
                ]);
            }
            Region::Masked { mask, skip, body } => {
                let source = match mask {
                    Mask::Input => 0,
                    Mask::Compare(k) => {
                        ops.push(Compare(*k));
                        VCC
                    }
                    Mask::Joined(k) => {
                        // s4's phi joins a VALU-derived mask with an input SGPR.
                        let b = ops.len();
                        ops.extend([
                            Compare(*k),
                            Mov(MASK, S(VCC)),
                            Less(1, 1),
                            Scc1(b + 5),
                            Mov(MASK, S(0)),
                        ]);
                        MASK
                    }
                };
                ops.push(Narrow { save, mask: source });
                let branch = ops.len();
                if *skip {
                    ops.push(ExecZ(0));
                }
                regions(body, ops, counter, save + 3);
                if *skip {
                    ops[branch] = ExecZ(ops.len());
                }
                ops.push(Mov(EXEC, S(save)));
            }
            Region::AdvanceTo(limit) => {
                // Save the enclosing mask once. Each trip permanently retires
                // the lanes that have reached the limit, until reconvergence.
                ops.push(Mov(save, S(EXEC)));
                let header = ops.len();
                ops.extend([
                    Below(*limit),
                    Narrow {
                        save: save + 3,
                        mask: VCC,
                    },
                    ExecZ(header + 6),
                    Vector(Alu::UpdateData),
                    Vector(Alu::Add(1)),
                    Jump(header),
                    Mov(EXEC, S(save)),
                ]);
            }
            Region::Repeat { times, body } => {
                ops.push(Mov(counter, Imm(0)));
                let header = ops.len();
                regions(body, ops, counter + 3, save);
                ops.extend([Inc(counter), Less(counter, *times), Scc1(header)]);
            }
        }
    }
}

pub(super) fn program(body: &[Region]) -> Vec<Op> {
    let mut ops = Vec::new();
    regions(body, &mut ops, 2, FIRST_SAVE);
    ops.extend([Observe, End]);
    ops
}

impl Src {
    fn bits(self) -> u32 {
        match self {
            S(r) => u32::from(r),
            Imm(k) => 128 + u32::from(k),
        }
    }
}

pub(super) fn encode(ops: &[Op]) -> Result<Vec<u8>, Stop> {
    let mut bytes = Vec::with_capacity(ops.len() * 4);
    for (pc, op) in ops.iter().enumerate() {
        let branch = |base: u32, t: usize| -> Result<u32, Stop> {
            if t >= ops.len() {
                return Err(Stop::Invalid("branch outside program"));
            }
            let displacement = i16::try_from(t as isize - pc as isize - 1)
                .map_err(|_| Stop::Invalid("branch out of encoding range"))?;
            Ok(base | u32::from(displacement as u16))
        };
        let word = match *op {
            Mov(r, src) => 0xbe80_0300 | u32::from(r) << 16 | src.bits(),
            Inc(r) => 0x8000_8100 | u32::from(r) << 16 | u32::from(r),
            Less(r, k) => 0xbf0a_0000 | Imm(k).bits() << 8 | u32::from(r),
            Vector(Alu::Add(k)) => (0x25 << 25) | Imm(k).bits(),
            Vector(Alu::AddData) => (0x25 << 25) | 257,
            Vector(Alu::UpdateData) => (0x25 << 25) | (1 << 17) | (1 << 9) | 256,
            Vector(Alu::XorData) => (0x1d << 25) | 257,
            Vector(Alu::Set(k)) => 0x7e00_0200 | Imm(k).bits(),
            Compare(k) => 0x7d82_0000 | Imm(k).bits(),
            Below(k) => 0x7d88_0000 | Imm(k).bits(),
            Narrow { save, mask } => 0xbe80_3c00 | u32::from(save) << 16 | u32::from(mask),
            Scc1(t) => branch(0xbf85_0000, t)?,
            ExecZ(t) => branch(0xbf88_0000, t)?,
            Jump(t) => branch(0xbf82_0000, t)?,
            Observe => 0x7e04_0300, // v_mov_b32 v2,v0
            End => 0xbf81_0000,
        };
        bytes.extend(word.to_le_bytes());
    }
    Ok(bytes)
}

// Independent register machine: never decodes bytes or consults compiler IR.
pub(super) fn direct(ops: &[Op], inputs: &Inputs) -> Result<[u32; 32], Stop> {
    let mut sgpr = [0u32; 128];
    sgpr[0] = inputs.mask;
    sgpr[1] = inputs.selector;
    sgpr[EXEC as usize] = u32::MAX;
    let mut v = inputs.v;
    let mut scc = false;
    let mut observed = false;
    let mut pc = 0;
    for _ in 0..LIMIT {
        let op = *ops
            .get(pc)
            .ok_or(Stop::Invalid("source PC out of bounds"))?;
        pc += 1;
        let exec = sgpr[EXEC as usize];
        match op {
            Mov(r, src) => {
                sgpr[r as usize] = match src {
                    S(s) => sgpr[s as usize],
                    Imm(k) => k.into(),
                }
            }
            Inc(r) => (sgpr[r as usize], scc) = sgpr[r as usize].overflowing_add(1),
            Less(r, k) => scc = sgpr[r as usize] < k.into(),
            Compare(k) | Below(k) => {
                let mut mask = 0u32;
                for (lane, value) in v[0].iter().enumerate() {
                    let passes = if matches!(op, Below(_)) {
                        *value < k.into()
                    } else {
                        *value > k.into()
                    };
                    if passes {
                        mask |= 1 << lane;
                    }
                }
                sgpr[VCC as usize] = mask & exec;
            }
            Narrow { save, mask } => {
                let narrowed = exec & sgpr[mask as usize];
                sgpr[save as usize] = exec;
                sgpr[EXEC as usize] = narrowed;
                scc = narrowed != 0;
            }
            Vector(a) => {
                let [accumulator, data, _] = &mut v;
                for (lane, (x, y)) in accumulator.iter_mut().zip(data.iter_mut()).enumerate() {
                    if exec & (1 << lane) != 0 {
                        match a {
                            Alu::Add(k) => *x = x.wrapping_add(k.into()),
                            Alu::AddData => *x = x.wrapping_add(*y),
                            Alu::UpdateData => *y = y.wrapping_add(*x),
                            Alu::XorData => *x ^= *y,
                            Alu::Set(k) => *x = k.into(),
                        };
                    }
                }
            }
            Scc1(t) if scc => pc = t,
            ExecZ(t) if exec == 0 => pc = t,
            Jump(t) => pc = t,
            Scc1(_) | ExecZ(_) => {}
            Observe => {
                if observed || exec != u32::MAX {
                    return Err(Stop::Invalid("observer must run once at full EXEC"));
                }
                v[2] = v[0];
                observed = true;
            }
            End if observed => return Ok(v[2]),
            End => return Err(Stop::Invalid("missing observer")),
        }
    }
    Err(Stop::Limit)
}
