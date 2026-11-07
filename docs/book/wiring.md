# Wiring

All signals are on header **J2**. Its silkscreen shows **GPIO numbers**, not pin
numbers: `2` is GPIO2 (J2 pin 7), `3` is GPIO3 (J2 pin 10).

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
check refuses them; see the [pin reference](reference/pins.md).
