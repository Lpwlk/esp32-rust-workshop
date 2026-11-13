//! Debouncing a jumper wire: what the firmware's main loop does with the times the
//! GPIO interrupt recorded.
//!
//! cargo run -p workshop-core --example button --target host-tuple

use workshop_core::button::{ButtonUpdate, Debouncer};

fn main() {
    // --8<-- [start:debounce]
    const MS: u64 = 1000; // times are µs
    let mut button = Debouncer::new(false, 30 * MS, 150 * MS); // 30 ms debounce, 150 ms lockout

    // The interrupt saw 3 edges (a bouncy contact), the last one at t = 12 ms.
    let (raw_edges, last_edge) = (3, 12 * MS);

    // The main loop polls; nothing is decided until the line has been quiet 30 ms.
    for now in [20 * MS, 30 * MS, 42 * MS] {
        if button.ready(now) && button.settled(last_edge, now) {
            // Line read low (= pressed) once it settled.
            match button.settle(true, raw_edges, now) {
                ButtonUpdate::Edge(ev) => println!("t={} ms: {:?}, {} raw edges", now / MS, ev.edge, ev.raw_edges),
                ButtonUpdate::Ignored { raw_edges } => println!("bounce ignored ({raw_edges} edges)"),
            }
        } else {
            println!("t={} ms: waiting for the line to settle", now / MS);
        }
    }
    assert!(button.is_pressed());
    // --8<-- [end:debounce]
}
