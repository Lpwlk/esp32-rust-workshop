//! A PWM generator on **GPIO20** (`D0` of header J2), driven from the computer's page (`pwm` command), to
//! feed the ADC with a known signal: wire `20` to `43` (through an RC filter to see the average instead
//! of the edges) and read it back in the spectrum.
//!
//! It is the RMT's transmitter in loop mode (esp-hal has no LEDC for the S31): one period of the wave sits in
//! the channel RAM as pulse codes and the hardware replays it with no CPU, at 80 MHz / divider. See
//! [`crate::pwm_plan`] for the arithmetic. 0.2 Hz to 1 MHz, any duty from 0 to 100 %; the frequency is exact to
//! half a tick (6 ns at 80 MHz, so 0.0005 % at 1 kHz).
//!
//! Changing the frequency or the duty restarts the wave (a glitch of one period at most). Off, the pin idles
//! low.

use esp_hal::{
    Blocking,
    gpio::Level,
    peripherals::{GPIO20, RMT},
    rmt::{Channel, ContinuousTxTransaction, LoopMode, PulseCode, Rmt, Tx, TxChannelConfig, TxChannelCreator},
    time::Rate,
};

use crate::pwm_plan::{self, CODES, SEGMENTS};

/// The output pin.
pub const PIN: u8 = 20;

/// What the generator does now.
#[derive(Clone, Copy, Debug)]
pub struct State {
    pub on: bool,
    /// What was asked, and what the hardware makes (a whole number of ticks).
    pub asked_hz: f32,
    pub hz: f32,
    pub duty: f32,
    /// Ticks in a period, and the tick's divider.
    pub period: u32,
    pub divider: u8,
}

pub struct Pwm {
    /// The channel when idle; `running` holds it while a wave plays.
    channel: Option<Channel<'static, Blocking, Tx>>,
    running: Option<ContinuousTxTransaction<'static>>,
    clock_hz: u32,
    state: State,
}

impl Pwm {
    /// Sets up RMT channel 0 on GPIO20 (idle low). `None` if the RMT refuses.
    pub fn new(rmt: RMT<'static>, pin: GPIO20<'static>) -> Option<Self> {
        let rmt = Rmt::new(rmt, Rate::from_mhz(80)).ok()?;
        let clock_hz = rmt.frequency().as_hz();
        let config =
            TxChannelConfig::default().with_clk_divider(1).with_idle_output(true).with_idle_output_level(Level::Low);
        let channel = rmt.channel0.configure_tx(&config).ok()?.with_pin(pin);
        Some(Self {
            channel: Some(channel),
            running: None,
            clock_hz,
            state: State { on: false, asked_hz: 1000.0, hz: 0.0, duty: 50.0, period: 0, divider: 1 },
        })
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// Stops the wave if there is one.
    fn stop(&mut self) {
        if let Some(tx) = self.running.take() {
            match tx.stop() {
                Ok(channel) | Err((_, channel)) => self.channel = Some(channel),
            }
        }
    }

    /// Plays `freq` Hz at `duty` % (or stops, with `on` false). The settings are remembered either way.
    pub fn apply(&mut self, freq: f32, duty: f32, on: bool) -> Result<State, &'static str> {
        let plan = pwm_plan::plan(self.clock_hz, freq, duty).ok_or("frequency out of range")?;
        self.stop();
        self.state.on = false;
        self.state.asked_hz = freq;
        self.state.hz = plan.freq();
        self.state.duty = plan.duty();
        self.state.period = plan.period;
        self.state.divider = plan.divider;
        if !on {
            return Ok(self.state);
        }

        let mut channel = self.channel.take().ok_or("generator busy")?;
        if channel
            .apply_config(
                &TxChannelConfig::default()
                    .with_clk_divider(plan.divider)
                    .with_idle_output(true)
                    .with_idle_output_level(Level::Low),
            )
            .is_err()
        {
            self.channel = Some(channel);
            return Err("RMT refused the divider");
        }

        let mut segments = [(false, 0u16); SEGMENTS];
        let n = pwm_plan::segments(&plan, &mut segments);
        let level = |high: bool| if high { Level::High } else { Level::Low };
        let mut codes = [PulseCode::end_marker(); CODES + 1];
        let (pairs, _) = segments[..n].as_chunks::<2>();
        for (code, pair) in codes.iter_mut().zip(pairs) {
            *code = PulseCode::new(level(pair[0].0), pair[0].1, level(pair[1].0), pair[1].1);
        }
        // (the code after the last pair is the end marker the loop needs)
        match channel.transmit_continuously(&codes[..n / 2 + 1], LoopMode::Infinite) {
            Ok(tx) => {
                self.running = Some(tx);
                self.state.on = true;
                Ok(self.state)
            }
            Err((_, channel)) => {
                self.channel = Some(channel);
                Err("RMT refused the wave")
            }
        }
    }
}
