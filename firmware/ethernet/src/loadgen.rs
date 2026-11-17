//! The traffic generator's glue (CLI `gen`): the schedule and the measurements are in
//! [`workshop_core::pktgen`]; this module carries commands to the network task, and what it has to say back.
//!
//! ```text
//!   CLI ── command() ──▶ COMMANDS ──▶ network task: Engine::poll() ──▶ UDP port net.port + 1 ──▶ peer
//!   CLI ◀─ take_report() ─ REPORTS ◀─ (status, end of a stream sent, end of a stream received)
//! ```
//!
//! The network task owns the [`Engine`]: it hands the datagrams the generator says are due to its socket, and
//! the datagrams that come in to the receiver. Both runtimes use it the same way (`rt` for Ferrite, `link` for
//! Embassy); only the socket calls differ. The traffic goes through the EMAC like any other, so the trace and its
//! dashboard see it.

use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use workshop_core::pktgen::{GenCmd, GenRx, Generator, RxSummary, Status, TxSummary};

/// What the network task tells the CLI.
pub enum Report {
    /// The state after a command.
    Status(Status),
    /// A command that was refused, and why.
    Error(&'static str),
    /// A stream this node sent has ended.
    Tx(TxSummary),
    /// A stream this node received has ended.
    Rx(RxSummary),
}

static COMMANDS: Channel<CriticalSectionRawMutex, GenCmd, 4> = Channel::new();
static REPORTS: Channel<CriticalSectionRawMutex, Report, 8> = Channel::new();

/// Sends a command to the generator. `false` if it is still busy with the previous ones.
pub fn command(cmd: GenCmd) -> bool {
    COMMANDS.try_send(cmd).is_ok()
}

/// The next thing the network task had to say, if any.
pub fn take_report() -> Option<Report> {
    REPORTS.try_receive().ok()
}

fn report(r: Report) {
    // Full: the CLI isn't reading, and an old report is worth less than the newest.
    if REPORTS.try_send(r).is_err() {
        let _ = REPORTS.try_receive();
    }
}

/// The generator and the receiver, owned by the network task.
pub struct Engine {
    generator: Generator,
    rx: GenRx,
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

impl Engine {
    /// An engine with nothing running.
    pub const fn new() -> Engine {
        Engine { generator: Generator::new(), rx: GenRx::new() }
    }

    /// Whether the task should come back soon (a stream is being sent or received) rather than at its leisure.
    pub fn is_busy(&self) -> bool {
        self.generator.is_active() || self.rx.is_receiving()
    }

    /// A datagram arrived on the generator's port.
    pub fn receive(&mut self, payload: &[u8], now_us: u64) {
        if let Some(summary) = self.rx.receive(payload, now_us) {
            report(Report::Rx(summary));
        }
    }

    /// Runs the commands that are waiting, then sends what is due (`send` returns whether the socket took it).
    pub fn poll(&mut self, now_us: u64, send: &mut dyn FnMut(&[u8]) -> bool) {
        while let Ok(cmd) = COMMANDS.try_receive() {
            // The seed of the random sizes and gaps: the clock is as good as anything.
            match self.generator.apply(cmd, now_us, now_us as u32 ^ 0x9E37_79B9) {
                Ok(()) => report(Report::Status(self.generator.status())),
                Err(why) => report(Report::Error(why)),
            }
        }
        self.generator.poll(now_us, send);
        if let Some(summary) = self.generator.take_summary() {
            report(Report::Tx(summary));
        }
        if let Some(summary) = self.rx.poll(now_us) {
            report(Report::Rx(summary));
        }
    }
}
