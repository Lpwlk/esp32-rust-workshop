//! Buttons on GPIO interrupts: the jumper wire on the breadboard, and the board's
//! BOOT button (GPIO61).
//!
//! This is the hardware glue. esp-hal has **one** interrupt handler for every
//! GPIO: [`on_gpio_edge`] checks each button's pin, and only records *that* its
//! line moved, and when (both edges). The debounce + lockout decision is made by
//! [`workshop_core::button::Debouncer`] (tested on the host), from the main loop.
//!
//! Both buttons are active low (pressed = line at GND): the jumper with the
//! internal pull-up, BOOT with the board's pull-up (the internal one is enabled
//! too, harmless).

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

/// Which button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// The jumper on the breadboard (`pins.button`).
    Jumper = 0,
    /// The BOOT button of the board (GPIO61).
    Boot = 1,
}

impl Source {
    /// Name shown in the console.
    pub fn name(self) -> &'static str {
        match self {
            Source::Jumper => "jumper",
            Source::Boot => "BOOT",
        }
    }
}

const COUNT: usize = 2;

/// Each button's pin, owned by the interrupt handler.
static PINS: Mutex<RefCell<[Option<Input<'static>>; COUNT]>> = Mutex::new(RefCell::new([None, None]));
/// Time of each button's last raw edge (µs since boot), `None` once handled.
static LAST_EDGE_US: Mutex<[Cell<Option<u64>>; COUNT]> = Mutex::new([Cell::new(None), Cell::new(None)]);
/// Raw edges (bounces included) of each button since its last decision.
static RAW_EDGES: [AtomicU32; COUNT] = [AtomicU32::new(0), AtomicU32::new(0)];

fn now_us() -> u64 {
    Instant::now().duration_since_epoch().as_micros()
}

#[handler]
fn on_gpio_edge() {
    let now = now_us();
    critical_section::with(|cs| {
        let mut pins = PINS.borrow_ref_mut(cs);
        let last = LAST_EDGE_US.borrow(cs);
        for (i, pin) in pins.iter_mut().enumerate() {
            if let Some(pin) = pin
                && pin.is_interrupt_set()
            {
                pin.clear_interrupt();
                RAW_EDGES[i].fetch_add(1, Ordering::Relaxed);
                last[i].set(Some(now));
            }
        }
    });
}

/// Owns the GPIO driver and its interrupt handler; create it once, then the buttons.
pub struct Gpio {
    _io: Io<'static>,
}

impl Gpio {
    pub fn new(io_mux: IO_MUX<'static>) -> Self {
        let mut io = Io::new(io_mux);
        io.set_interrupt_handler(on_gpio_edge);
        Self { _io: io }
    }
}

pub struct Button {
    source: Source,
    debouncer: Debouncer,
}

impl Button {
    /// Takes `pin` for `source` and starts listening to both edges. `_gpio` proves
    /// the interrupt handler is installed.
    pub fn new(
        _gpio: &Gpio,
        source: Source,
        pin: impl InputPin + 'static,
        debounce: Duration,
        lockout: Duration,
    ) -> Self {
        let mut input = Input::new(pin, InputConfig::default().with_pull(Pull::Up));
        let pressed = input.is_low();
        critical_section::with(|cs| {
            input.listen(Event::AnyEdge);
            PINS.borrow_ref_mut(cs)[source as usize].replace(input);
        });
        Self { source, debouncer: Debouncer::new(pressed, debounce.as_micros(), lockout.as_micros()) }
    }

    pub fn source(&self) -> Source {
        self.source
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
        let i = self.source as usize;
        let now = now_us();
        if !self.debouncer.ready(now) {
            // Lockout running: edges keep being recorded, evaluated once it ends.
            return None;
        }
        let level_low = critical_section::with(|cs| {
            let last = &LAST_EDGE_US.borrow(cs)[i];
            match last.get() {
                Some(t) if self.debouncer.settled(t, now) => {
                    last.set(None);
                    PINS.borrow_ref(cs)[i].as_ref().map(|p| p.is_low())
                }
                _ => None,
            }
        })?;
        let raw_edges = RAW_EDGES[i].swap(0, Ordering::Relaxed);
        Some(self.debouncer.settle(level_low, raw_edges, now))
    }
}
