//! Continuous ADC sampling by DMA: the SAR ADC's own timer triggers every conversion and a DMA channel
//! writes the results to memory, with no CPU involved until they are read.
//!
//! This is what ESP-IDF's `adc_continuous` driver does on the ESP32-S31 (`esp_hal_ana_conv/esp32s31/.../adc_ll.h`,
//! `esp_adc/gdma/adc_dma.c`), written here against the registers because esp-hal has no continuous ADC
//! for the S31 (and doesn't expose the DMA used for it, the **LP AHB GDMA**).
//!
//! ```text
//!  GPIO43 ─▶ SAR1 ─▶ digital controller (pattern table, timer: one conversion every `timer_target` cycles)
//!  GPIO44 ─▶  ▲            │  4 bytes per conversion: [16:0] data, [20:17] channel, [21] unit
//!                          ▼
//!                  LP AHB GDMA, RX channel 0 ──▶ ring of descriptors ──▶ buffers in RAM ──▶ [`AdcDma::drain`]
//! ```
//!
//! Timing comes from the ADC's timer (the 40 MHz crystal divided), so the sampling instants are exactly
//! regular, and the chip's limits are those of ESP-IDF: 611 Hz to 83 333 Hz.
//!
//! The potentiometer ([`crate::pot`], GPIO44) shares the ADC: when it is on, the pattern table has two entries
//! and the controller converts the signal pin and the pot's pin in turn. The signal keeps its regular instants
//! (every other conversion), at half the conversion rate; [`AdcDma::drain`] tells the two apart by the channel
//! number of each result.
//!
//! The descriptors form a ring of [`DESCRIPTORS`] buffers of [`PER_DESCRIPTOR`] conversions. The hardware
//! hands a filled one to the CPU (owner bit cleared); [`AdcDma::drain`] reads them in order and gives them
//! back. If the CPU is too slow the DMA runs out of descriptors and stops: `drain` notices, restarts it
//! and counts a stall (the samples in between are lost).

use core::cell::UnsafeCell;

use esp_hal::{
    Blocking,
    analog::adc::{Adc, AdcConfig, AdcPin, Attenuation},
    peripherals::{ADC1, APB_SARADC, GPIO43, GPIO44, LP_PERI},
};
use esp32s31::LP_AHB_DMA;

/// The channel of GPIO43 on ADC1.
pub const CHANNEL: u8 = 1;
/// The channel of the potentiometer's GPIO44 on ADC1.
pub const POT_CHANNEL: u8 = crate::pot::CHANNEL;
/// The ADC's digital controller runs from the 40 MHz crystal.
const XTAL_HZ: u64 = 40_000_000;
/// Sample rate limits of the digital controller (`SOC_ADC_SAMPLE_FREQ_THRES_*` of the S31).
pub const MIN_RATE_HZ: u32 = 611;
pub const MAX_RATE_HZ: u32 = 83_333;
/// Peripheral number of the ADC for the GDMA (`SOC_GDMA_TRIG_PERIPH_ADC0`).
const GDMA_PERIPH_ADC: u8 = 0;

/// Conversions per descriptor (and per DMA end-of-frame): 1 KiB buffers.
pub const PER_DESCRIPTOR: usize = 256;
/// Descriptors in the ring: 32 × 256 conversions is 98 ms at 83 kS/s.
pub const DESCRIPTORS: usize = 32;

/// One DMA descriptor (`dma_descriptor_t`): three words.
/// `dw0`: [11:0] buffer size, [23:12] bytes received, [30] end of frame, [31] owner (1 = the DMA).
#[repr(C, align(4))]
#[derive(Clone, Copy)]
struct Descriptor {
    dw0: u32,
    buffer: u32,
    next: u32,
}

const OWNER_DMA: u32 = 1 << 31;
const BUFFER_BYTES: u32 = (PER_DESCRIPTOR * 4) as u32;

/// Memory the DMA writes to: `UnsafeCell` so the statics can be `Sync`; only this module touches it.
struct Shared<T>(UnsafeCell<T>);
unsafe impl<T> Sync for Shared<T> {}

static DESC: Shared<[Descriptor; DESCRIPTORS]> =
    Shared(UnsafeCell::new([Descriptor { dw0: 0, buffer: 0, next: 0 }; DESCRIPTORS]));
static BUFFERS: Shared<[[u32; PER_DESCRIPTOR]; DESCRIPTORS]> =
    Shared(UnsafeCell::new([[0; PER_DESCRIPTOR]; DESCRIPTORS]));

/// The ADC, its pin and the DMA channel.
pub struct AdcDma {
    /// Kept alive: they hold the ADC's clock and the pin's analog mode.
    _adc: Adc<'static, ADC1<'static>, Blocking>,
    _pin: AdcPin<GPIO43<'static>, ADC1<'static>>,
    _pot_pin: AdcPin<GPIO44<'static>, ADC1<'static>>,
    /// Next descriptor to read.
    next: usize,
    running: bool,
    /// Times the DMA ran out of descriptors (or its FIFO overflowed) and had to be restarted.
    pub stalls: u32,
    /// DMA buffers handed over since the start of the program (for the verbose statistics).
    pub buffers: u32,
    /// The potentiometer's conversions since the stream started, and the last whole result word (`pot` shows them).
    pub pot_conversions: u32,
    pub pot_last_word: u32,
}

impl AdcDma {
    /// Sets up the pin, the ADC's clocks and the LP AHB GDMA (nothing is sampled until [`start`](Self::start)).
    pub fn new(gpio: GPIO43<'static>, pot_gpio: GPIO44<'static>, adc1: ADC1<'static>) -> Self {
        // esp-hal's driver does the clocks, the reference generator and the power-up; its one-shot
        // mode isn't used afterwards.
        let mut config = AdcConfig::new();
        let pin = config.enable_pin(gpio, Attenuation::_11dB);
        let pot_pin = config.enable_pin(pot_gpio, Attenuation::_11dB);
        let adc = Adc::new(adc1, config).expect("ADC1");

        // The DMA: bus clock, reset, and the memory range it may access (LPMEM, SRAM, ROM, flash, PSRAM).
        let clk = LP_PERI::regs();
        clk.ahb_dma_ctrl().modify(|_, w| w.lp_ahb_dma_clk_en().set_bit());
        clk.ahb_dma_ctrl().modify(|_, w| w.lp_ahb_dma_rst_en().set_bit());
        clk.ahb_dma_ctrl().modify(|_, w| w.lp_ahb_dma_rst_en().clear_bit());
        let dma = dma();
        dma.ahb_dma_misc_conf().modify(|_, w| w.ahb_dma_clk_en().set_bit());
        dma.ahb_dma_intr_mem_start_addr()
            .write(|w| unsafe { w.ahb_dma_access_intr_mem_start_addr().bits(0x2E00_0000) });
        dma.ahb_dma_intr_mem_end_addr().write(|w| unsafe { w.ahb_dma_access_intr_mem_end_addr().bits(0x53FF_FFFF) });

        Self {
            _adc: adc,
            _pin: pin,
            _pot_pin: pot_pin,
            next: 0,
            running: false,
            stalls: 0,
            buffers: 0,
            pot_conversions: 0,
            pot_last_word: 0,
        }
    }

    /// Starts sampling the signal at about `rate_hz`, and the potentiometer too when `pot` is set (the conversions
    /// alternate, so the controller runs at twice the rate: the chip's limits apply to that). Returns the exact
    /// rate of the signal, in millihertz: the timer divides 40 MHz by integers.
    pub fn start(&mut self, rate_hz: u32, pot: bool) -> u32 {
        if self.running {
            self.stop();
        }
        let pins = if pot { 2 } else { 1 };
        self.pot_conversions = 0;
        self.pot_last_word = 0;
        let rate = u64::from((rate_hz * pins).clamp(MIN_RATE_HZ, MAX_RATE_HZ));
        // The digital controller's clock is the crystal divided by `div`; it triggers a conversion every
        // `interval` cycles of its half-rate clock (a 12-bit counter), as in ESP-IDF's
        // `adc_hal_digi_sample_freq_config`.
        let max_ctrl = XTAL_HZ.min(rate * 2 * 4095);
        let div = XTAL_HZ.div_ceil(max_ctrl).max(1);
        let interval = XTAL_HZ / (div * 2 * rate);

        let adc = APB_SARADC::regs();
        let clk = LP_PERI::regs();
        // Controller clock: crystal, divided by `div`.
        clk.adc_ctrl().modify(|_, w| unsafe {
            w.lp_adc_clk_sel().bits(1);
            w.lp_adc_div_num().bits((div - 1) as u8)
        });
        adc.ctrl_date().modify(|_, w| w.clk_en().set_bit());

        // ADC1 only, scan mode (the controller's timer triggers it), one pattern entry: our channel.
        adc.ctrl0().modify(|_, w| unsafe {
            w.sar1_trigger_stop().clear_bit();
            w.sar1_trigger_start().clear_bit();
            w.sar1_continue_mode_en().clear_bit();
            w.sar1_patt_type().clear_bit();
            w.sar1_patt_len().bits(pins as u8 - 1);
            w.sar1_trigger_mode().bits(1)
        });
        adc.ctrl1().modify(|_, w| unsafe { w.sar2_trigger_mode().bits(0) });
        // An entry is six bits, the first in bits 23:18 and each next one six bits lower (`adc_ll_digi_set_pattern_table`).
        let table = (u32::from(CHANNEL) << 2) << 18 | if pot { (u32::from(POT_CHANNEL) << 2) << 12 } else { 0 };
        adc.sar1_patt_tab1().write(|w| unsafe { w.sar1_patt_tab1().bits(table) });
        adc.ctrl0().modify(|_, w| w.sar1_patt_p_clear().set_bit());
        adc.ctrl0().modify(|_, w| w.sar1_patt_p_clear().clear_bit());
        adc.ctrl2().modify(|_, w| unsafe { w.timer_target().bits(interval as u16) });

        // IDF's order: stop the ADC, the DMA and the link between them; reset both; link the descriptors;
        // start the DMA, connect the ADC to it, and only then start the ADC's timer.
        adc.ctrl2().modify(|_, w| w.timer_en().clear_bit());
        adc.dma_conf().modify(|_, w| w.adc_trans().clear_bit());
        let dma = dma();
        dma.ahb_dma_in_link_ch(0).modify(|_, w| w.ahb_dma_inlink_stop_ch().set_bit());
        dma.ahb_dma_in_conf0_ch(0).modify(|_, w| w.ahb_dma_in_rst_ch().set_bit());
        dma.ahb_dma_in_conf0_ch(0).modify(|_, w| w.ahb_dma_in_rst_ch().clear_bit());
        adc.dma_conf().modify(|_, w| unsafe { w.adc_eof_num().bits(PER_DESCRIPTOR as u16) });
        adc.dma_conf().modify(|_, w| w.dma_eof_num_clr().set_bit());
        adc.dma_conf().modify(|_, w| w.dma_eof_num_clr().clear_bit());

        self.link_descriptors();
        dma.ahb_dma_in_pri_ch(0).modify(|_, w| unsafe { w.ahb_dma_rx_pri_ch().bits(0) });
        dma.ahb_dma_in_peri_sel_ch(0).modify(|_, w| unsafe { w.ahb_dma_peri_in_sel_ch().bits(GDMA_PERIPH_ADC) });
        dma.ahb_dma_in_conf0_ch(0).modify(|_, w| w.ahb_dma_mem_trans_en_ch().clear_bit());
        dma.ahb_dma_in_conf1_ch(0).modify(|_, w| w.ahb_dma_in_check_owner_ch().set_bit());
        clear_flags();
        let first = DESC.0.get() as u32;
        dma.ahb_dma_in_link_addr_ch(0).modify(|_, w| unsafe { w.ahb_dma_inlink_addr_ch().bits(first) });
        dma.ahb_dma_in_link_ch(0).modify(|_, w| w.ahb_dma_inlink_start_ch().set_bit());
        adc.dma_conf().modify(|_, w| w.adc_trans().set_bit());
        adc.ctrl2().modify(|_, w| w.timer_en().set_bit());

        self.running = true;
        (XTAL_HZ * 1000 / (div * 2 * interval) / u64::from(pins)) as u32
    }

    /// Stops sampling.
    pub fn stop(&mut self) {
        let adc = APB_SARADC::regs();
        adc.ctrl2().modify(|_, w| w.timer_en().clear_bit());
        adc.dma_conf().modify(|_, w| w.adc_trans().clear_bit());
        dma().ahb_dma_in_link_ch(0).modify(|_, w| w.ahb_dma_inlink_stop_ch().set_bit());
        self.running = false;
    }

    /// Hands every conversion that has arrived to `sink` (the signal's) or `pot_sink` (the potentiometer's), oldest
    /// first (the 17-bit code, 0 to 4393), and gives the buffers back to the DMA. Returns `true` if the DMA had
    /// stalled (some samples are missing).
    pub fn drain(&mut self, mut sink: impl FnMut(u16), mut pot_sink: impl FnMut(u16)) -> bool {
        if !self.running {
            return false;
        }
        loop {
            // SAFETY: only this module touches the descriptors; the DMA owns one until it clears the owner bit.
            let desc = unsafe { &mut (*DESC.0.get())[self.next] };
            let dw0 = unsafe { core::ptr::read_volatile(&desc.dw0) };
            if dw0 & OWNER_DMA != 0 {
                break;
            }
            core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
            let words = (((dw0 >> 12) & 0xFFF) / 4) as usize;
            let buffer = unsafe { &(*BUFFERS.0.get())[self.next] };
            for word in &buffer[..words.min(PER_DESCRIPTOR)] {
                let result = unsafe { core::ptr::read_volatile(word) };
                let code = (result & 0x1_FFFF) as u16;
                if (result >> 17) & 0xF == u32::from(POT_CHANNEL) {
                    self.pot_conversions = self.pot_conversions.wrapping_add(1);
                    self.pot_last_word = result;
                    pot_sink(code);
                } else {
                    sink(code);
                }
            }
            core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
            unsafe { core::ptr::write_volatile(&mut desc.dw0, BUFFER_BYTES | OWNER_DMA) };
            self.next = (self.next + 1) % DESCRIPTORS;
            self.buffers = self.buffers.wrapping_add(1);
        }

        // Out of descriptors (or the FIFO overflowed): the DMA stopped. Everything is free again, so restart it.
        let raw = dma().ahb_dma_in_int_raw_ch(0).read();
        let stalled =
            raw.ahb_dma_in_dscr_empty_ch_int_raw().bit_is_set() || raw.ahb_dma_infifo_ovf_ch_int_raw().bit_is_set();
        if stalled {
            clear_flags();
            dma().ahb_dma_in_link_ch(0).modify(|_, w| w.ahb_dma_inlink_restart_ch().set_bit());
            self.stalls += 1;
        }
        stalled
    }

    /// The ring of descriptors, all owned by the DMA and empty.
    fn link_descriptors(&mut self) {
        let descs = unsafe { &mut *DESC.0.get() };
        let buffers = unsafe { &mut *BUFFERS.0.get() };
        let base = descs.as_ptr() as u32;
        for (i, d) in descs.iter_mut().enumerate() {
            d.dw0 = BUFFER_BYTES | OWNER_DMA;
            d.buffer = buffers[i].as_ptr() as u32;
            d.next = base + ((i + 1) % DESCRIPTORS * core::mem::size_of::<Descriptor>()) as u32;
        }
        self.next = 0;
        core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
    }
}

fn dma() -> LP_AHB_DMA {
    // SAFETY: the LP AHB GDMA belongs to this driver (esp-hal doesn't use it).
    unsafe { LP_AHB_DMA::steal() }
}

/// Clears the "no descriptor" and "FIFO overflow" flags of RX channel 0.
fn clear_flags() {
    dma().ahb_dma_in_int_clr_ch(0).write(|w| {
        w.ahb_dma_in_dscr_empty_ch_int_clr().set_bit();
        w.ahb_dma_infifo_ovf_ch_int_clr().set_bit();
        w.ahb_dma_in_suc_eof_ch_int_clr().set_bit();
        w.ahb_dma_in_done_ch_int_clr().set_bit()
    });
}
