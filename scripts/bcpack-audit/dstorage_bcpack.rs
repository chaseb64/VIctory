//! Source-generated BC1 streams delivered through the conformance title.

use super::{
    Com, E_PENDING, Hresult, Request, S_OK, SOURCE_FILE, Storage, Tap, ZLIB, close, enqueue,
    enqueue_status, from_file, status, submit, wait,
};

const BC1: u64 = 1 << 1;
const INVALID_DATA: Hresult = 0x8007_000d_u32 as i32;
const INIT: u8 = 0xa5;
const GUARD: usize = 16;
const ZERO_BC1: [u8; 8] = [0, 0, 0, 0, 0xaa, 0xaa, 0xaa, 0xaa];
const TWO_BC1: [u8; 16] = [
    0, 0, 0, 0, 0xaa, 0xaa, 0xaa, 0xaa, 0, 8, 0, 0, 0xaa, 0xaa, 0xaa, 0xaa,
];

/// Shape 0 is uniform, with slot zero reserved. Each lane ends its only
/// fragment after one zero, or two symbols when testing the control stream.
fn inverse(state: u32, frequency: u32, start: u32) -> u32 {
    ((state / frequency) << 16) + state % frequency + start
}

/// Encode one record of zero symbols, or two records with +1 red for the
/// second block's first endpoint. Rank 0 in each quadrant gives index 2.
fn bc1_wire(two_blocks: bool) -> Vec<u8> {
    let widths = [7, 7, 4, 5, 4, 4, 5, 4, 7, 7, 7, 7];
    let mut lanes = [[0u8; 7]; 12];
    for (i, (&width, lane)) in widths.iter().zip(&mut lanes).enumerate() {
        let narrowing = if two_blocks { width - 4 } else { 0 };
        let frequency = (65536 >> (width - narrowing + 1)) - 1;
        let end = 1u32 << 24;
        let (state, refill) = if two_blocks {
            let tail = if i == 2 {
                inverse(end, 2048, 4096) // symbol 2: zigzag(+1) in lane 2
            } else {
                inverse(end, frequency, 1)
            };
            (inverse(tail >> 8, frequency, 1), tail as u8)
        } else {
            // Wide lanes can legally end at 2^16. The skipped low byte is zero.
            let end = if end >= frequency << 16 {
                end >> 8
            } else {
                end
            };
            (inverse(end, frequency, 1), 0)
        };
        lane[0] = 0x10 | (narrowing << 6);
        lane[1] = 3 + u8::from(two_blocks);
        lane[2..6].copy_from_slice(&state.to_be_bytes());
        lane[6] = refill;
    }

    // For these fixed-length fragments every lane needs a byte on each
    // clock, so the two feeder groups run independently.
    let mut wire = Vec::new();
    for group in lanes.chunks(8) {
        for byte in 0..(6 + usize::from(two_blocks)) {
            wire.extend(group.iter().map(|lane| lane[byte]));
        }
    }
    wire
}

struct Guarded {
    bytes: [u8; GUARD * 2 + 16],
    size: usize,
}

impl Guarded {
    fn new(size: usize) -> Self {
        Self {
            bytes: [INIT; GUARD * 2 + 16],
            size,
        }
    }

    fn destination(&mut self) -> &mut [u8] {
        &mut self.bytes[GUARD..GUARD + self.size]
    }

    fn check(&self, t: &mut Tap, label: &str, hr: Hresult, expected_hr: Hresult, expected: &[u8]) {
        let middle = &self.bytes[GUARD..GUARD + self.size];
        let guards_intact = self.bytes[..GUARD]
            .iter()
            .chain(&self.bytes[GUARD + self.size..])
            .all(|&byte| byte == INIT);
        let name = format!("dstorage bcpack audit: {label}");
        t.check(hr == expected_hr && guards_intact && middle == expected, &name, || {
            format!(
                "HRESULT {hr:#010x} (expected {expected_hr:#010x}), guards intact {guards_intact}, destination {middle:02x?} (expected {expected:02x?})"
            )
        });
    }
}

#[path = "texture_pairs.rs"]
mod texture_pairs;

fn deliver(
    t: &mut Tap,
    s: &Storage,
    array: Com,
    format: u64,
    packed: &[u8],
    expected: &[u8],
    zlib: bool,
    label: &str,
) {
    let queue = s.queue(32);
    let mut output = vec![INIT; GUARD + expected.len() + GUARD];
    let compressed;
    let (flags, source) = if zlib {
        compressed = miniz_oxide::deflate::compress_to_vec_zlib(packed, 6);
        (ZLIB, compressed.as_slice())
    } else {
        (0, packed)
    };
    let mut request = Request::memory(
        (format << 1) | flags,
        source,
        &mut output[GUARD..GUARD + expected.len()],
    );
    if zlib {
        request.intermediate_size = packed.len() as u32;
    }
    enqueue(queue, &request);
    enqueue_status(queue, array, 0);
    submit(queue);
    let hr = wait(array, 0, 5000);
    close(queue);
    queue.release();
    let interior = &output[GUARD..GUARD + expected.len()];
    let guards = output[..GUARD]
        .iter()
        .chain(&output[GUARD + expected.len()..])
        .all(|&b| b == INIT);
    t.check(hr == S_OK && interior == expected && guards, label,
        || format!("HRESULT {hr:#010x}, guards intact {guards}, destination {interior:02x?}, expected {expected:02x?}"));
}

fn format_pairs(t: &mut Tap, s: &Storage, array: Com) {
    for &(format, packed, expected) in texture_pairs::PAIRS {
        for (stage, zlib) in [("raw", false), ("zlib", true)] {
            let label = format!("dstorage bcpack audit: format {format} {stage} two-block request");
            deliver(t, s, array, format, packed, expected, zlib, &label);
        }
    }
    // BC1 red endpoint deltas -16 then +15. Every quadrant uses selector 2.
    deliver(
        t,
        s,
        array,
        1,
        texture_pairs::CARRY,
        &[
            0, 128, 0, 0, 170, 170, 170, 170, 0, 120, 0, 0, 170, 170, 170, 170,
        ],
        false,
        "dstorage bcpack audit: two-block split refill",
    );
}

pub(super) fn run(t: &mut Tap, s: &Storage, array: Com) {
    format_pairs(t, s, array);
    let raw = bc1_wire(true);
    let starvation = bc1_wire(false);
    let wrapped = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6);
    let mut memory = raw.clone();
    memory.extend([0xff; 16]);

    let raw_offset = 32;
    let mut contents = vec![0xcc; raw_offset];
    contents.extend_from_slice(&raw);
    contents.resize(contents.len().next_multiple_of(16), 0xdd);
    let wrapped_offset = contents.len();
    contents.extend_from_slice(&wrapped);
    contents.extend([0xee; 16]);
    let path = std::env::temp_dir().join(format!(
        "victory-conformance-bcpack-{}.bin",
        std::process::id()
    ));
    if let Err(error) = std::fs::write(&path, &contents) {
        t.check(false, "dstorage bcpack audit: open synthetic file", || {
            error.to_string()
        });
        return;
    }
    let file = match s.open(&path) {
        Ok(file) => file,
        Err(hr) => {
            t.check(false, "dstorage bcpack audit: open synthetic file", || {
                format!("{hr:#010x}")
            });
            let _ = std::fs::remove_file(&path);
            return;
        }
    };
    t.check(
        true,
        "dstorage bcpack audit: open synthetic file",
        String::new,
    );

    let mem = s.queue(32);
    let normal = s.queue_of(SOURCE_FILE, 0, 32);
    let low = s.queue_of(SOURCE_FILE, -1, 32);
    let mut memory_raw = Guarded::new(16);
    let mut rejected = Guarded::new(16);
    let mut memory_wrapped = Guarded::new(16);
    let mut file_raw = Guarded::new(16);
    let mut file_wrapped = Guarded::new(16);
    let mut one_block = Guarded::new(8);
    let mut repeat = Guarded::new(16);

    // The descriptor is reused immediately. The memory buffers and file
    // object stay alive through completion of all requests that use them.
    let mut request = Request::memory(BC1, &memory, memory_raw.destination());
    request.source_size = raw.len() as u32;
    enqueue(mem, &request);
    enqueue_status(mem, array, 0);
    request = Request::memory(BC1 | ZLIB, &wrapped, rejected.destination());
    request.intermediate_size = (raw.len() - 1) as u32;
    enqueue(mem, &request);
    enqueue_status(mem, array, 1);
    request = Request::memory(BC1 | ZLIB, &wrapped, memory_wrapped.destination());
    request.intermediate_size = raw.len() as u32;
    enqueue(mem, &request);
    enqueue_status(mem, array, 2);

    enqueue(
        normal,
        &from_file(
            BC1,
            file,
            raw_offset as u64,
            raw.len(),
            file_raw.destination(),
        ),
    );
    enqueue_status(normal, array, 3);
    request = from_file(
        BC1 | ZLIB,
        file,
        wrapped_offset as u64,
        wrapped.len(),
        file_wrapped.destination(),
    );
    request.intermediate_size = (raw.len() + 4096) as u32;
    enqueue(low, &request);
    enqueue_status(low, array, 4);
    enqueue(
        mem,
        &Request::memory(BC1, &starvation, one_block.destination()),
    );
    enqueue_status(mem, array, 5);

    let all_pending = (0..=5).all(|index| status(array, index) == E_PENDING);
    submit(mem);
    let memory_statuses = [0, 1, 2, 5].map(|index| wait(array, index, 5000));
    let file_pending = [3, 4]
        .iter()
        .all(|&index| status(array, index) == E_PENDING);
    submit(normal);
    submit(low);
    let file_statuses = [3, 4].map(|index| wait(array, index, 5000));

    let (repeat_pending, repeat_status) = if memory_statuses[3] != E_PENDING {
        enqueue(mem, &Request::memory(BC1, &raw, repeat.destination()));
        enqueue_status(mem, array, 0);
        let pending = status(array, 0) == E_PENDING;
        submit(mem);
        (pending, wait(array, 0, 5000))
    } else {
        (false, E_PENDING)
    };
    for queue in [mem, normal, low] {
        close(queue);
        queue.release();
    }
    file.release();
    let removed = std::fs::remove_file(&path);
    t.check(
        removed.is_ok(),
        "dstorage bcpack audit: release file after completion",
        || format!("{removed:?}"),
    );
    t.check(all_pending && file_pending, "dstorage bcpack audit: pending status is isolated across queues", || {
        format!("initially pending {all_pending}, file queues pending after memory submission {file_pending}")
    });
    t.check(
        repeat_pending,
        "dstorage bcpack audit: reused status resets to pending",
        String::new,
    );
    memory_raw.check(
        t,
        "memory BCPack from a declared prefix",
        memory_statuses[0],
        S_OK,
        &TWO_BC1,
    );
    rejected.check(
        t,
        "short zlib intermediate fails before destination writes",
        memory_statuses[1],
        INVALID_DATA,
        &[INIT; 16],
    );
    memory_wrapped.check(
        t,
        "memory zlib then BCPack with exact intermediate",
        memory_statuses[2],
        S_OK,
        &TWO_BC1,
    );
    file_raw.check(
        t,
        "normal-priority file BCPack subrange",
        file_statuses[0],
        S_OK,
        &TWO_BC1,
    );
    file_wrapped.check(
        t,
        "low-priority file zlib then BCPack with spare intermediate capacity",
        file_statuses[1],
        S_OK,
        &TWO_BC1,
    );
    repeat.check(
        t,
        "repeated BCPack on the same queue and status entry",
        repeat_status,
        S_OK,
        &TWO_BC1,
    );
    one_block.check(
        t,
        "single-block entropy lane completion",
        memory_statuses[3],
        S_OK,
        &ZERO_BC1,
    );
}
