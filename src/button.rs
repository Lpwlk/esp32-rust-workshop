//! "Button" made of a jumper wire: GPIO4 with internal pull-up, shorted to GND to press.
//!
//! The GPIO interrupt fires on both edges. A jumper bounces a lot when it touches
//! the breadboard, so the interrupt only records *that* something happened and
//! when. [`Button::poll`] then waits for the line to be quiet for the debounce time
//! before reading the settled level and reporting a single clean edge, then
//! ignores further changes during a lockout. A jumper sliding into a breadboard
//! can lose contact for longer than the debounce time; the lockout filters that,
//! and the level is read again when it ends, so a real change is never missed.

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

/// What [`Button::poll`] observed.
pub enum ButtonUpdate {
    /// A real, debounced level change.
    Edge(ButtonEvent),
    /// Edges that ended on the level already reported (bounce or lockout): ignored.
    Ignored { raw_edges: u32 },
}

pub struct Button {
    _io: Io<'static>,
    pressed: bool,
    /// Line must be stable this long after the last edge.
    debounce: Duration,
    /// Changes ignored this long after an accepted edge.
    lockout: Duration,
    /// Set after an accepted edge; changes are ignored until then.
    lockout_until: Option<Instant>,
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

        Self { _io: io, pressed, debounce, lockout, lockout_until: None }
    }

    /// Changes the filter timings (from the CLI); takes effect on the next edge.
    pub fn set_timing(&mut self, debounce: Duration, lockout: Duration) {
        self.debounce = debounce;
        self.lockout = lockout;
    }

    pub fn is_pressed(&self) -> bool {
        self.pressed
    }

    /// Call often from the main loop. Reports an edge once the line has settled
    /// on a level different from the last reported one, outside the lockout.
    pub fn poll(&mut self) -> Option<ButtonUpdate> {
        if let Some(until) = self.lockout_until {
            if Instant::now() < until {
                // Edges keep being recorded by the interrupt; they're evaluated below
                // once the lockout is over.
                return None;
            }
            self.lockout_until = None;
        }

        let level_low = critical_section::with(|cs| {
            let last = LAST_EDGE.borrow(cs);
            match last.get() {
                Some(t) if t.elapsed() >= self.debounce => {
                    last.set(None);
                    PIN.borrow_ref(cs).as_ref().map(|p| p.is_low())
                }
                _ => None,
            }
        })?;

        let raw_edges = RAW_EDGES.swap(0, Ordering::Relaxed);
        if level_low == self.pressed {
            return Some(ButtonUpdate::Ignored { raw_edges });
        }
        self.pressed = level_low;
        self.lockout_until = Some(Instant::now() + self.lockout);
        Some(ButtonUpdate::Edge(ButtonEvent {
            edge: if level_low { Edge::Falling } else { Edge::Rising },
            raw_edges,
        }))
    }
}
