//! The Step 12 generalization proof: one exported function whose outcome is decided by its
//! argument alone, so a harness must reach three different outcomes through the same plugin
//! contract the JavaScript target uses.
//!
//! `len == 7` traps on an index one past the end, `len == 99` never returns, and a length above
//! `u32::MAX / 3` traps on the arithmetic that `overflow-checks` keeps live. Everything else
//! returns, and the branches in between are what the coverage digest has to show.

/// The probe's only entry point. `len` is a `u32` because that is what a core wasm module can
/// carry: the driver in the harness constructs the argument from the same type IR the
/// JavaScript plugin uses for a `number`.
#[no_mangle]
pub extern "C" fn probe(len: u32) -> u32 {
    if len == 7 {
        let rows = len as usize;
        let table = vec![0u32; rows];
        // The finding: the table holds `len` cells and this reads the one after them.
        return table[rows];
    }

    if len == 99 {
        // The deadline finding. `black_box` keeps the loop, because a spin with no effect is
        // something the optimizer is entitled to remove, and a removed spin is an `ok`.
        let mut ticks: u64 = 0;
        loop {
            ticks = ticks.wrapping_add(1);
            std::hint::black_box(ticks);
        }
    }

    if len % 2 == 0 {
        // An even length is scaled by the batch factor.
        len * 3 + 1
    } else {
        // An odd length goes through the offset path instead.
        len * 5 - 2
    }
}
