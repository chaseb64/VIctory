# Wave32 execution equivalence

This native CPU integration test compares all 32 dwords written by a final
`v_mov_b32 v2,v0`. The source must reach that observer once after restoring
full EXEC. Distinct initial values are assigned to v0, v1, and v2. The checker
does not compare intermediate EXEC, VCC, SCC, scalar scratch, branch traces,
or instruction counts. A wave can execute a block that some structured lanes skip.

## Language and independence

The original generator concatenates integer operations, uniform two-arm
branches, masked regions and counted loops with one through six trips.
The targeted generator adds two levels of mask narrowing with independent
restores, nested counted loops, and loops that retire lanes on different
trips. A counted loop resets its own scalar counter on entry. A divergent
loop saves the enclosing EXEC once, narrows it to lanes with v0 below a small
constant, updates v1 and v0 on the surviving lanes and takes the backedge.
When EXEC reaches zero, it restores the enclosing mask. Masked regions can
either branch over the body on empty EXEC or execute guarded writes.

- v0 and v1 carry per-lane integers. Operations set or increment v0, add or
  xor v1 into v0, or add v0 into v1. v2 observes the final v0. Addition wraps
  modulo 2^32. Inline constants are in 0..=64.
- s0 supplies an arbitrary mask and s1 selects a uniform branch. Counted loops
  use s2 and s5. Nested regions save masks in s3, s6, s9 and s12. s4 holds a mask
  chosen by a uniform branch. A vector comparison considers only active lanes.
  EXEC initially includes every lane. Other scalar inputs start at zero.
- The source executor interprets typed instructions with absolute branch targets.
  It holds scalar dwords and full arrays of lane values, and updates only active
  lanes. Encoding translates the source instructions to little-endian GFX10
  words with signed relative branch displacements.
- The SSA evaluator consumes `ir::build(bytes, false, 3)`, the decoded fields,
  actual operand bindings, phis, predecessor lists and exits. The structured
  evaluator consumes `structure::structure(&program)`, walking individual lanes
  through the returned exits and guards with the boolean-node and mask-phi
  mappings. Neither transformed evaluator sees the source instructions.
- Shared pieces are the input fixture, observation contract and standard Rust
  integer operations. The generator and reference share the source instruction
  stream. Hand examples check what that stream means. This is differential
  coverage of the named ISA subset, not an independent certification of the ISA.

The executors bound dynamic execution to 1,024 source instructions or 1,024
block visits (per lane for structured execution). Floating point, Wave64, memory,
textures, barriers, derivatives, discards, cross-lane instructions, arbitrary
scalar side effects in divergent regions, mask-as-data uses and emitted host
shader code are outside the grammar.

## Run and replay

From the checkout with its pinned Rust toolchain:

```sh
cargo test -p victory-shader --locked --test equivalence
cargo test -p victory-shader --locked --test equivalence --release search::campaign -- --exact --ignored --nocapture
WAVE_SEED=42 WAVE_COUNT=1 cargo test -p victory-shader --locked --test equivalence --release search::campaign -- --exact --ignored --nocapture
WAVE_SEED=0 WAVE_COUNT=10000 cargo test -p victory-shader --locked --test equivalence --release search::targeted_campaign -- --exact --ignored --nocapture
```

Each unsigned decimal seed reconstructs the program and inputs from SplitMix64.
Use the same campaign name with `WAVE_COUNT=1` to replay a seed. The original
campaign retains its previous seed mapping. The targeted campaign selects an
interaction by `seed % 4` and varies the masks, skip branches, loop limits,
data and uniform selector within it.

| Remainder | Interaction |
| --- | --- |
| 0 | A uniform mask join encloses a second narrowing and a sibling write after the inner restore. |
| 1 | A masked vector update sits inside two counted loops carrying dependent v0 and v1 values. |
| 2 | A divergent loop retires lanes inside an input mask and restores it before a sibling write. |
| 3 | A counted loop re-enters a divergent loop under two saved masks after a uniform branch changes v0. |

The regular tests require seeds 10,000 through 10,127 from each generator to
complete and agree at both stages. Each opt-in campaign runs at most 10,000
consecutive seeds or 30 minutes and reports per-stage agreement counts.
The targeted campaign requires the entire requested range to agree.
Refusal, unsupported behavior, invalid execution, compiler errors, exhaustion,
panic and differences are counted separately. A mismatch prints its seed,
stage, first differing lane, expected and actual dwords and reduced regions.

Reduction rebuilds branch targets from the grammar after every edit. It removes
regions at any nesting level and decreases loop bounds while retaining the
same stage, lane, expected dword and actual dword. It produces a local minimum
under those edits. A controlled guard fault tests the reducer even when a campaign
finds no compiler mismatch. Other in-memory mutations exercise the SSA loop
backedge, inactive-lane masking and the structured branch destinations.

Additional fault controls reset the backedge of the second live vector phi,
force a divergent loop to exit early and reuse an inner write guard after the
parent mask was restored. The nested-guard witness is reduced and replayed
both with the injected fault and with the original transformation.
