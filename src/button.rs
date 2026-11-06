//! "Button" made of a jumper wire: a GPIO with internal pull-up, shorted to GND to press.
//!
//! This is the hardware glue. The GPIO interrupt fires on both edges and only
//! records *that* the line moved, and when. The debounce + lockout decision is
//! made by [`workshop_core::button::Debouncer`] (tested on the host), from the
//! main loop.

use core::cell::{Cell, RefCell};
use core::sync::atomic::{AtomicU32, Ordering};

use critical_section::Mutex;
use esp_hal::{
    gpio::{Event, Input, InputConfig, InputPin, Io, Pull},
    handler,
    peripherals::IO_MUX,
    time::{Duration, Instant},
};
use workshop_core::button::Debouncer;
pub use workshop_core::button::{ButtonEvent, ButtonUpdate};

static PIN: Mutex<RefCell<Option<Input<'static>>>> = Mutex::new(RefCell::new(None));
/// Time of the last raw edge seen by the interrupt (µs since boot), `None` once handled.
static LAST_EDGE_US: Mutex<Cell<Option<u64>>> = Mutex::new(Cell::new(None));
/// Raw edges (including bounces) since the last decision.
static RAW_EDGES: AtomicU32 = AtomicU32::new(0);

fn now_us() -> u64 {
    Instant::now().duration_since_epoch().as_micros()
}

#[handler]
fn on_gpio_edge() {
    critical_section::with(|cs| {
        let mut pin = PIN.borrow_ref_mut(cs);
        let Some(pin) = pin.as_mut() else { return };
        if pin.is_interrupt_set() {
            pin.clear_interrupt();
            RAW_EDGES.fetch_add(1, Ordering::Relaxed);
            LAST_EDGE_US.borrow(cs).set(Some(now_us()));
        }
    });
}

pub struct Button {
    _io: Io<'static>,
    debouncer: Debouncer,
}

impl Button {
    pub fn new(io_mux: IO_MUX<'static>, pin: impl InputPin + 'static, debounce: Duration, lockout: Duration) -> Self {
        let mut io = Io::new(io_mux);
        io.set_interrupt_handler(on_gpio_edge);

        let mut input = Input::new(pin, InputConfig::default().with_pull(Pull::Up));
        let pressed = input.is_low();
        critical_section::with(|cs| {
            input.listen(Event::AnyEdge);
            PIN.borrow_ref_mut(cs).replace(input);
        });

        Self { _io: io, debouncer: Debouncer::new(pressed, debounce.as_micros(), lockout.as_micros()) }
    }

    /// Changes the filter timings (from the CLI); takes effect on the next edge.
    pub fn set_timing(&mut self, debounce: Duration, lockout: Duration) {
        self.debouncer.set_timing(debounce.as_micros(), lockout.as_micros());
    }

    pub fn is_pressed(&self) -> bool {
        self.debouncer.is_pressed()
    }

    /// Call often from the main loop: returns a decision once the line has settled.
    pub fn poll(&mut self) -> Option<ButtonUpdate> {
        let now = now_us();
        if !self.debouncer.ready(now) {
            // Lockout running: edges keep being recorded, evaluated once it ends.
            return None;
        }
        let level_low = critical_section::with(|cs| {
            let last = LAST_EDGE_US.borrow(cs);
            match last.get() {
                Some(t) if self.debouncer.settled(t, now) => {
                    last.set(None);
                    PIN.borrow_ref(cs).as_ref().map(|p| p.is_low())
                }
                _ => None,
            }
        })?;
        let raw_edges = RAW_EDGES.swap(0, Ordering::Relaxed);
        Some(self.debouncer.settle(level_low, raw_edges, now))
    }
}
