# Remote debugging (Raspberry Pi)

The boards can be plugged into a Raspberry Pi and used from your computer over SSH.
Build on your computer (much faster), flash and watch on the Pi.

## On the Pi, once

```sh
cargo install espflash --locked
sudo usermod -aG dialout "$USER"      # serial ports, then log in again
```

For probe-rs (on-target tests, debugger), also install its udev rules
(see the [probe-rs docs](https://probe.rs/docs/getting-started/probe-setup/)).

## Flash and monitor through SSH

```sh
cargo build --release --bin node-a
scp target/riscv32imafc-unknown-none-elf/release/node-a pi:
ssh -t pi espflash flash --monitor --chip esp32s31 node-a
```

`ssh -t` keeps the terminal interactive, so the CLI works as if the board were on
your desk. Run it inside `tmux` on the Pi to keep both monitors open when you
disconnect.

## Debugger

On the Pi, `probe-rs dap-server --port 50000`; in VS Code, point the probe-rs
launch configuration to that server. The ELF must also be on the Pi.

!!! tip "Power"
    Two boards on the Pi's USB ports can draw a lot. If they reset at random, use
    a powered USB hub.
