// Fixture generator for the VIctory validation campaign. Upstream bcpack-rs
// supplies the synthetic encoder and independent contiguous-stream decoder.
use bcpack_reference::{encode, rans};
use std::{collections::HashSet, env};

// Layout entries hold the BC format number, symbols per lane per block, and alphabet bits minus one.
const LAYOUTS: &[(u8, usize, &[u8])] = &[
    (1, 1, &[7, 7, 4, 5, 4, 4, 5, 4, 7, 7, 7, 7]),
    (3, 2, &[3, 3, 7, 5, 5, 5, 5, 4, 5, 4, 7, 7]),
    (4, 1, &[7, 7, 7, 7, 5, 5, 5, 5, 5, 5, 5, 5]),
    (5, 2, &[3, 3, 7, 7, 5, 5, 5, 5, 5, 5, 5, 5]),
    (6, 2, &[3, 3, 7, 7, 7, 4, 4, 4, 5, 5, 5, 7, 7, 7, 7, 4]),
    (7, 2, &[3, 3, 7, 7, 7, 7, 7, 6, 6, 4, 5, 7, 7, 7, 7, 4]),
];

fn generate(widths: &[u8], count: usize, seed: u64, limit: u64) -> (Vec<u8>, Vec<Vec<u16>>) {
    let mut rng = encode::Rng(seed);
    let mut lanes = Vec::new();
    let mut expected = Vec::new();
    for &width in widths {
        let mut symbols = Vec::new();
        let mut fragments = Vec::new();
        while symbols.len() < count {
            let n = (1 + rng.below(limit) as usize).min(count - symbols.len());
            let q = rng.below(16) as u8;
            let z = rng.below(4) as u8;
            let alphabet = 2u64 << (width - z);
            let part: Vec<_> = (0..n).map(|_| rng.below(alphabet) as u16).collect();
            symbols.extend_from_slice(&part);
            fragments.push(encode::Fragment {
                symbols: part,
                q,
                z,
            });
        }
        lanes.push(fragments);
        expected.push(symbols);
    }
    (encode::encode_lanes(widths, &lanes).unwrap(), expected)
}

fn carry_case(widths: &[u8]) -> (Vec<u8>, Vec<Vec<u16>>) {
    let mut intended = vec![vec![0, 0]; widths.len()];
    let mut lane_bytes: Vec<_> = widths
        .iter()
        .map(|&width| {
            encode::encode_fragment(
                width,
                &encode::Fragment {
                    symbols: vec![0, 0],
                    q: 0,
                    z: 0,
                },
            )
            .unwrap()
        })
        .collect();
    // Frequency-one symbols 31 and 30 start at 65535 and 65534. Decoding
    // the first leaves 256. Its next refill spans a non-reset fragment boundary.
    intended[2] = vec![31, 30];
    lane_bytes[2] = vec![0x0c, 4, 1, 0, 255, 255, 255, 0x1d, 0, 254];
    let wire = encode::interleave(widths, 2, &lane_bytes).unwrap();
    (wire, intended)
}

#[derive(Default)]
struct Counts {
    cases: usize,
    blocks: usize,
    fragments: u64,
    errors: usize,
    wrong: usize,
    symbols: usize,
}

impl Counts {
    fn check(&mut self, name: &str, widths: &[u8], per: usize, wire: &[u8], intended: &[Vec<u16>]) {
        let target = intended[0].len();
        let blocks = target / per;
        let external = rans::decode_slice(wire, widths, target).unwrap();
        assert_eq!(
            external.consumed,
            wire.len(),
            "{name} reference consumption"
        );
        for (lane, symbols) in intended.iter().enumerate() {
            assert_eq!(external.lane(lane), symbols, "{name} reference lane {lane}");
        }
        let mut expected = Vec::with_capacity(target * widths.len());
        for block in 0..blocks {
            for lane in intended {
                expected.extend(
                    lane[block * per..(block + 1) * per]
                        .iter()
                        .map(|&s| u8::try_from(s).unwrap()),
                );
            }
        }
        let mut records = vec![0; expected.len()];
        match victory_subject::replay(widths, per, blocks, wire, &mut records) {
            Err(err) => {
                self.errors += 1;
                if self.errors + self.wrong <= 12 {
                    println!("{name}: {err}");
                }
            }
            Ok(()) if expected != records => {
                self.wrong += 1;
                if self.errors + self.wrong <= 12 {
                    println!("{name}: different records");
                }
            }
            Ok(()) => {}
        }
        self.cases += 1;
        self.blocks += blocks;
        self.fragments += external
            .stats
            .iter()
            .map(|lane| u64::from(lane.fragments))
            .sum::<u64>();
        self.symbols += expected.len();
    }
}

fn main() {
    let expected: usize = env::args()
        .nth(1)
        .expect("expected disagreement count")
        .parse()
        .unwrap();
    let mut counts = Counts::default();
    for &(bc, per, widths) in LAYOUTS {
        for (blocks, seed) in [(1, 11), (2, 23), (17, 37), (257, 53), (512, 71)] {
            let (wire, symbols) = generate(widths, blocks * per, seed + u64::from(bc), 39);
            counts.check(&format!("bc{bc}-{blocks}"), widths, per, &wire, &symbols);
        }
    }
    let (_, per, widths) = LAYOUTS[0];
    let (wire, symbols) = carry_case(widths);
    counts.check("bc1-split-refill", widths, per, &wire, &symbols);
    let additional: usize = env::args().nth(2).map(|v| v.parse().unwrap()).unwrap_or(0);
    let mut unique = HashSet::new();
    for i in 0..additional {
        let (bc, per, widths) = LAYOUTS[i % LAYOUTS.len()];
        let blocks = 1 + ((i / LAYOUTS.len()) % 12);
        let seed = 0x4a58_3a69_0000_0000u64 + i as u64;
        let (wire, symbols) = generate(widths, blocks * per, seed, 6);
        counts.check(
            &format!("bc{bc}-seed-{seed:016x}"),
            widths,
            per,
            &wire,
            &symbols,
        );
        assert!(unique.insert(wire), "duplicate generated wire at index {i}");
    }
    println!(
        "{} streams; {} blocks; {} symbols; {} fragments; {} subject errors; {} record mismatches",
        counts.cases, counts.blocks, counts.symbols, counts.fragments, counts.errors, counts.wrong
    );
    assert_eq!(counts.errors + counts.wrong, expected);
}
