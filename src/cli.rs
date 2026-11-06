//! CLI on the USB Serial/JTAG port (the one `espflash monitor` uses): the USB glue.
//!
//! Line editing, parsing, the help table and the `burst` questions are in
//! [`workshop_core::cli`] (re-exported here), tested on the host. This module reads
//! key presses from USB, feeds them to the editor and keeps the prompt line on
//! screen (see `term`).

use esp_hal::{
    Blocking,
    peripherals::USB_DEVICE,
    usb::usb_serial_jtag::{UsbSerialJtag, UsbSerialJtagRx, UsbSerialJtagTx},
};
pub use workshop_core::cli::*;
use workshop_core::config::BurstConfig;

use crate::term;

pub struct Cli {
    rx: UsbSerialJtagRx<'static, Blocking>,
    // Output goes through esp-println; the TX half is only kept so the driver stays alive.
    _tx: UsbSerialJtagTx<'static, Blocking>,
    editor: LineEditor,
    /// `burst` typed alone: the next lines answer its questions.
    wizard: Option<BurstWizard>,
}

impl Cli {
    pub fn new(usb: USB_DEVICE<'static>) -> Self {
        let (rx, tx) = UsbSerialJtag::new(usb).split();
        term::prompt_enable();
        Self { rx, _tx: tx, editor: LineEditor::new(), wizard: None }
    }

    /// Asks the `burst` parameters; the answers come back as a [`Command::Burst`].
    pub fn start_burst_wizard(&mut self, defaults: BurstConfig) {
        let wizard = BurstWizard::new(defaults);
        term::prompt_label(Some(&wizard.question()));
        self.wizard = Some(wizard);
    }

    /// Processes pending key presses, from USB and (feature `rtt`) from the probe's
    /// terminal. Returns a command when Enter completes a valid line.
    pub fn poll(&mut self) -> Option<Command> {
        let mut rtt = [0u8; 32];
        let rtt_len = crate::rtt_console::read(&mut rtt);
        let mut rtt_bytes = rtt[..rtt_len].iter().copied();
        while let Some(byte) = self.rx.read_byte().ok().or_else(|| rtt_bytes.next()) {
            // In the wizard, Enter on an empty line means "take the default".
            if self.wizard.is_some() && matches!(byte, b'\r' | b'\n') && self.editor.line().is_empty() {
                if let Some(cmd) = self.answer("") {
                    return Some(cmd);
                }
                continue;
            }
            match self.editor.feed(byte) {
                Some(EditorEvent::Changed) => term::prompt_set(self.editor.line(), self.editor.cursor()),
                Some(EditorEvent::Submitted(line)) => {
                    term::prompt_set("", 0);
                    if self.wizard.is_some() {
                        // Answers to questions don't go in the history.
                        if let Some(cmd) = self.answer(&line) {
                            return Some(cmd);
                        }
                        continue;
                    }
                    // Remembered even if invalid: ↑ brings it back to fix it.
                    self.editor.remember(&line);
                    term::cli_echo(&line);
                    match parse(&line) {
                        Ok(cmd) => return Some(cmd),
                        Err(e) => term::cli_error(format_args!("{e}")),
                    }
                }
                None => {}
            }
        }
        None
    }

    fn answer(&mut self, line: &str) -> Option<Command> {
        let wizard = self.wizard.as_mut()?;
        term::cli_answer(&wizard.question(), line);
        match wizard.answer(line) {
            WizardStep::Ask => {
                term::prompt_label(Some(&wizard.question()));
                None
            }
            WizardStep::Retry(msg) => {
                term::cli_error(format_args!("{msg}"));
                None
            }
            WizardStep::Done(cmd) => {
                self.wizard = None;
                term::prompt_label(None);
                Some(cmd)
            }
            WizardStep::Cancelled => {
                self.wizard = None;
                term::prompt_label(None);
                term::cli_ok(format_args!("burst cancelled"));
                None
            }
        }
    }
}
