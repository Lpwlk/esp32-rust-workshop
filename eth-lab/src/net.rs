//! Ethernet bring-up: the PHY, the EMAC driver, and the embassy-net stack with this node's
//! static address (there is no DHCP).
//!
//! The board's RJ45 is wired to a Motorcomm YT8531 PHY over RGMII (GPIO5-19, see
//! `workshop_core::net::PHY_PINS`). The driver is esp-hal's `ethernet` module; embassy-net
//! hides what the PHY negotiated, so [`SpeedTap`] wraps the driver and records the link speed
//! and duplex each time the stack asks for the link state, for [`link_state`].

use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, Ordering};

use embassy_net::{Config, Ipv4Address, Ipv4Cidr, Runner, Stack, StackResources, StaticConfigV4};
use embassy_net_driver::{Capabilities, Driver, HardwareAddress, LinkState as DriverLink};
use embassy_time::{Duration, Timer};
use esp_hal::{
    Async,
    ethernet::{Ethernet, EthernetDmaStorage, RgmiiPinBundle, mac::Duplex, mac::Speed, phy::generic::GenericPhy},
    gpio::{Level, Output, OutputConfig},
    peripherals::{
        ETH, GPIO5, GPIO6, GPIO7, GPIO8, GPIO9, GPIO10, GPIO11, GPIO12, GPIO13, GPIO14, GPIO15, GPIO16, GPIO17, GPIO18,
        GPIO19,
    },
};
use static_cell::{ConstStaticCell, StaticCell};
use workshop_core::config::NetLink;

type Inner = Ethernet<'static, Async, GenericPhy>;

/// The driver the stack runs on.
pub type EthDriver = SpeedTap;

/// Frame buffers of the EMAC DMA: 32 to receive, 16 to send. A burst arrives faster than the main
/// loop polls (it also prints), so the receive ring has to absorb what comes in between.
static DMA: ConstStaticCell<EthernetDmaStorage<32, 16>> = ConstStaticCell::new(EthernetDmaStorage::new());
static RESOURCES: StaticCell<StackResources<3>> = StaticCell::new();
/// Keeps the PHY's reset line driven high for the life of the firmware.
static RESET: StaticCell<Output<'static>> = StaticCell::new();

/// What the PHY reported at the last link check (0 = no link).
static MBPS: AtomicU16 = AtomicU16::new(0);
/// When the PHY was last read for its speed (ms since boot).
static LAST_READ_MS: AtomicU32 = AtomicU32::new(0);
/// The speed is re-read at most this often: every read is several slow MDIO transactions, and
/// the stack asks for the link state on every one of its polls (that is, on every packet).
const READ_EVERY_MS: u32 = 500;
static FULL_DUPLEX: AtomicBool = AtomicBool::new(false);

/// The PHY's state as last seen by the stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhyLink {
    /// Cable connected and partner detected.
    pub up: bool,
    /// Negotiated speed, Mbit/s (0 when down).
    pub mbps: u16,
    /// Negotiated duplex.
    pub full_duplex: bool,
}

/// The PHY link, as of the stack's last check (the stack polls the PHY about every second).
pub fn link_state() -> PhyLink {
    let mbps = MBPS.load(Ordering::Relaxed);
    PhyLink { up: mbps != 0, mbps, full_duplex: FULL_DUPLEX.load(Ordering::Relaxed) }
}

/// Wraps the EMAC driver and records the negotiated speed and duplex (embassy-net's own link
/// state is only up or down). Everything else is forwarded unchanged.
pub struct SpeedTap(Inner);

impl Driver for SpeedTap {
    type RxToken<'a>
        = <Inner as Driver>::RxToken<'a>
    where
        Self: 'a;
    type TxToken<'a>
        = <Inner as Driver>::TxToken<'a>
    where
        Self: 'a;

    fn receive(&mut self, cx: &mut core::task::Context<'_>) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        Driver::receive(&mut self.0, cx)
    }

    fn transmit(&mut self, cx: &mut core::task::Context<'_>) -> Option<Self::TxToken<'_>> {
        Driver::transmit(&mut self.0, cx)
    }

    fn link_state(&mut self, cx: &mut core::task::Context<'_>) -> DriverLink {
        // The inner call sets the MAC's speed and duplex and the DMA interrupts; then read what it saw.
        let state = Driver::link_state(&mut self.0, cx);
        match state {
            DriverLink::Up => {
                let now = (esp_hal::time::Instant::now().duration_since_epoch().as_millis() & 0xFFFF_FFFF) as u32;
                let known = MBPS.load(Ordering::Relaxed) != 0;
                if known && now.wrapping_sub(LAST_READ_MS.load(Ordering::Relaxed)) < READ_EVERY_MS {
                    return state;
                }
                LAST_READ_MS.store(now, Ordering::Relaxed);
                let phy = self.0.poll_link(None);
                let mbps = match phy.speed {
                    Speed::_10M => 10,
                    Speed::_100M => 100,
                    Speed::_1000M => 1000,
                    _ => 0,
                };
                MBPS.store(mbps, Ordering::Relaxed);
                FULL_DUPLEX.store(phy.duplex == Duplex::Full, Ordering::Relaxed);
            }
            DriverLink::Down => MBPS.store(0, Ordering::Relaxed),
        }
        state
    }

    fn capabilities(&self) -> Capabilities {
        Driver::capabilities(&self.0)
    }

    fn hardware_address(&self) -> HardwareAddress {
        Driver::hardware_address(&self.0)
    }
}

/// The peripherals of the PHY: the EMAC and GPIO5-19 (`workshop_core::net::PHY_PINS`).
#[allow(missing_docs)]
pub struct PhyPeripherals {
    pub eth: ETH<'static>,
    pub mdc: GPIO5<'static>,
    pub mdio: GPIO6<'static>,
    pub reset: GPIO7<'static>,
    pub txd0: GPIO8<'static>,
    pub txd1: GPIO9<'static>,
    pub txd2: GPIO10<'static>,
    pub txd3: GPIO11<'static>,
    pub tx_ctl: GPIO12<'static>,
    pub tx_clk: GPIO13<'static>,
    pub rx_clk: GPIO14<'static>,
    pub rx_ctl: GPIO15<'static>,
    pub rxd3: GPIO16<'static>,
    pub rxd2: GPIO17<'static>,
    pub rxd1: GPIO18<'static>,
    pub rxd0: GPIO19<'static>,
}

/// This node's MAC address: the chip's unique base MAC with the last byte + 3, as ESP-IDF does
/// for the Ethernet interface, so two boards never clash.
pub fn mac_address() -> [u8; 6] {
    let mut mac = [0u8; 6];
    mac.copy_from_slice(esp_hal::efuse::base_mac_address().as_bytes());
    mac[5] = mac[5].wrapping_add(3);
    mac
}

/// Resets the PHY, starts the EMAC and the network stack with `net`'s static address.
/// Returns the stack (for the sockets) and its runner, which must be spawned (see [`net_task`]).
pub async fn start(p: PhyPeripherals, net: &NetLink, seed: u64) -> (Stack<'static>, Runner<'static, EthDriver>) {
    // PHY reset (GPIO7): low for 100 ms, then 300 ms for it to come up.
    let mut reset = Output::new(p.reset, Level::Low, OutputConfig::default());
    Timer::after(Duration::from_millis(100)).await;
    reset.set_high();
    Timer::after(Duration::from_millis(300)).await;

    let eth = Ethernet::new(
        p.eth,
        DMA.take(),
        mac_address(),
        // The PHY's address on the MDIO bus is found by scanning.
        GenericPhy::new_auto(),
        RgmiiPinBundle {
            rx_clk: p.rx_clk,
            tx_clk: p.tx_clk,
            rx_ctl: p.rx_ctl,
            tx_ctl: p.tx_ctl,
            rxd0: p.rxd0,
            rxd1: p.rxd1,
            rxd2: p.rxd2,
            rxd3: p.rxd3,
            txd0: p.txd0,
            txd1: p.txd1,
            txd2: p.txd2,
            txd3: p.txd3,
            mdc: p.mdc,
            mdio: p.mdio,
        },
    )
    .expect("Ethernet init failed")
    .into_async();

    let config = Config::ipv4_static(StaticConfigV4 {
        address: Ipv4Cidr::new(Ipv4Address::from(net.address), net.prefix_len),
        gateway: None,
        dns_servers: heapless::Vec::new(),
    });
    RESET.init(reset);
    embassy_net::new(SpeedTap(eth), config, RESOURCES.init(StackResources::new()), seed)
}

/// Runs the network stack: spawn it once.
#[embassy_executor::task]
pub async fn net_task(mut runner: Runner<'static, EthDriver>) -> ! {
    runner.run().await
}
