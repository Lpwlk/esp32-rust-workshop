//! "Button" made of a jumper wire: GPIO4 with internal pull-up, shorted to GND to press.
//!
//! The GPIO interrupt fires on both edges. A jumper bounces a lot when it touches
//! the breadboard, so the interrupt only records *that* something happened and
//! when. [`Button::poll`] then waits for the line to be quiet for [`DEBOUNCE`]
//! before reading the settled level and reporting a single clean edge.

use core::cell::{Cell, RefCell};
use core::sync::atomic::{AtomicU32, Ordering};

use critical_section::Mutex;
use esp_hal::{
    gpio::{Event, Input, InputConfig, InputPin, Io, Pull},
    handler,
    peripherals::IO_MUX,
    time::{Duration, Instant},
};

use crate::protocol::Edge;

/// The line must stay stable this long after the last edge to be accepted.
const DEBOUNCE: Duration = Duration::from_millis(30);

static PIN: Mutex<RefCell<Option<Input<'static>>>> = Mutex::new(RefCell::new(None));
/// Time of the last raw edge seen by the interrupt, `None` once handled.
static LAST_EDGE: Mutex<Cell<Option<Instant>>> = Mutex::new(Cell::new(None));
/// Raw edges (including bounces) since the last reported event.
static RAW_EDGES: AtomicU32 = AtomicU32::new(0);

#[handler]
fn on_gpio_edge() {
    critical_section::with(|cs| {
        let mut pin = PIN.borrow_ref_mut(cs);
        let Some(pin) = pin.as_mut() else { return };
        if pin.is_interrupt_set() {
            pin.clear_interrupt();
            RAW_EDGES.fetch_add(1, Ordering::Relaxed);
            LAST_EDGE.borrow(cs).set(Some(Instant::now()));
        }
    });
}

/// A debounced button event.
pub struct ButtonEvent {
    pub edge: Edge,
    /// Raw interrupts counted for this event (1 = clean contact, more = bounces).
    pub raw_edges: u32,
}

pub struct Button {
    _io: Io<'static>,
    pressed: bool,
}

impl Button {
    pub fn new(io_mux: IO_MUX<'static>, pin: impl InputPin + 'static) -> Self {
        let mut io = Io::new(io_mux);
        io.set_interrupt_handler(on_gpio_edge);

        let mut input = Input::new(pin, InputConfig::default().with_pull(Pull::Up));
        let pressed = input.is_low();
        critical_section::with(|cs| {
            input.listen(Event::AnyEdge);
            PIN.borrow_ref_mut(cs).replace(input);
        });

        Self { _io: io, pressed }
    }

    pub fn is_pressed(&self) -> bool {
        self.pressed
    }

    /// Call often from the main loop. Returns an event once the line has settled
    /// on a level different from the last reported one.
    pub fn poll(&mut self) -> Option<ButtonEvent> {
        let level_low = critical_section::with(|cs| {
            let last = LAST_EDGE.borrow(cs);
            match last.get() {
                Some(t) if t.elapsed() >= DEBOUNCE => {
                    last.set(None);
                    PIN.borrow_ref(cs).as_ref().map(|p| p.is_low())
                }
                _ => None,
            }
        })?;

        let raw_edges = RAW_EDGES.swap(0, Ordering::Relaxed);
        if level_low == self.pressed {
            // Bounced and came back to the same level: no real change.
            return None;
        }
        self.pressed = level_low;
        Some(ButtonEvent {
            edge: if level_low { Edge::Falling } else { Edge::Rising },
            raw_edges,
        })
    }
}
