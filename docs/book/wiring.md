# Wiring

All signals are on header **J2**. Its silkscreen shows **GPIO numbers**, not pin
numbers: `2` is GPIO2 (J2 pin 7), `3` is GPIO3 (J2 pin 10).

There are two ways to link the boards, and you only wire one of them: the **UART** (three wires, below) or the
**Ethernet** cable (the RJ45 ports, [further down](#ethernet-link-between-the-boards)). The button and the LEDs are
wired the same way for both.

```text
     Board A  (node-a)                  Board B  (node-b)
  ┌────────────────────────┐        ┌────────────────────────┐
  │ GPIO2  TX ─────────────┼───────▶│ GPIO3  RX              │
  │ GPIO3  RX ◀────────────┼────────│ GPIO2  TX              │
  │ GND ───────────────────┼────────│ GND                    │
  │ GPIO4 button           │        │ GPIO4 button           │
  │ GPIO20/21 LEDs         │        │ GPIO20/21 LEDs         │
  └───────────┬────────────┘        └───────────┬────────────┘
              │ USB Serial/JTAG                 │ USB Serial/JTAG
              └────────── your computer ────────┘
                 (flash, console + CLI, probe-rs)
```

## UART link between the boards

TX of one board goes to RX of the other, and the grounds are shared:

| Board A | | Board B |
|---|---|---|
| `2` GPIO2 (TX) | → | `3` GPIO3 (RX) |
| `3` GPIO3 (RX) | ← | `2` GPIO2 (TX) |
| `G` GND | — | `G` GND |

!!! warning "5 Mbaud needs short wires"
    One bit lasts 200 ns. Keep the jumper wires short (a few cm), and always
    connect the grounds. `framing error` or `glitch on RX line` messages in the
    console point at the wiring.

## Ethernet link between the boards

For the Ethernet app, plug a cable from the **RJ45** port of one board to the RJ45 port of the other. There is nothing
to wire on header J2:

```text
   Board A  192.168.77.11                    Board B  192.168.77.12
  ┌──────────────┐        cat5e (straight)  ┌──────────────┐
  │ RJ45 ════════╪══════════════════════════╪════════ RJ45 │      no switch, no DHCP
  └──────────────┘                          └──────────────┘
```

- A straight cable is enough: the PHYs detect the cable type (Auto-MDIX). A cat5e or better cable, with all four
  pairs, is needed for 1 Gbit/s; a poorer one links at 100 Mbit/s.
- The addresses are fixed in the [presets](presets.md) of each node. A computer on the same cable (or a switch)
  needs an address in `192.168.77.0/24` to talk to a board.
- The on-board PHY (Motorcomm YT8531, RGMII) is wired to **GPIO5-19**: MDC 5, MDIO 6, reset 7, data and clocks
  8-19. The configuration check refuses those pins for anything else
  ([Ethernet pins](reference/eth-pins.md)); the usual pins of this page (button 4, LEDs 20 and 21) don't clash.
- USB-C (USB Serial/JTAG) stays your console and flashing port, as for the UART app.

## Jumper button

GPIO4 has its internal pull-up enabled. A wire from `4` (J2 pin 32) to `G`
(J2 pin 33, right next to it):

- plugged: line low, **pressed**, `FALL` event;
- removed: line high, **released**, `RISE` event.

The board's **BOOT** button (GPIO61, nothing to wire) works the same way: press
it for a `FALL`, release it for a `RISE`. Each `EVT` line in the console says which
button sent it (`jumper` or `BOOT`), and `DAT` frames report "pressed" while
either one is held. BOOT keeps its usual role at power-up: held during a reset, it
starts the ROM's download mode instead of the firmware.

## Breadboard LEDs

Each LED: GPIO → 330 Ω resistor → LED anode (long leg), cathode → GND. That's
about 4 mA for a red LED at 3.3 V.

| LED | GPIO | J2 label |
|---|---|---|
| LED 1 | 20 | `D0` |
| LED 2 | 21 | `D1` |

What each LED shows is set by `led1` / `led2` (see the
[settings](reference/config.md)): peer's button, link state, RX/TX activity...

## Pins to avoid

Some GPIOs are used by the module (flash, USB) or don't exist. The configuration
check refuses them; see the [pin reference](reference/pins.md) (and the
[Ethernet one](reference/eth-pins.md), which adds GPIO5-19).
