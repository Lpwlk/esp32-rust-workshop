# adc-stream: the ADC of one board, streamed to a computer

A third firmware, next to the two board-to-board apps ([Ethernet](../ethernet), [UART](../uart)): **one**
ESP32-S31 board samples the analog pin **GPIO43** (`ADC1_CH1`, pin « 43 » of header J2) at up to **83 kS/s**
and sends the samples over Ethernet (UDP) to a computer, where a Plotly page draws the spectrum, the
waveform and a waterfall, with the board's log in a terminal.

```sh
python3 scripts/dev.py setup                  # once: numpy and plotly go in the .venv
python3 scripts/dev.py --app adc flash a -m   # build, flash node A's board (or b) (192.168.77.20, UDP port 7100), open its console
python3 scripts/dev.py fft                    # on the computer: the page, at http://127.0.0.1:8050
python3 scripts/dev.py fft --demo             # the page with a simulated board (no hardware needed)
```

The computer's Ethernet port (a USB-C adapter is fine) needs an address in the board's subnet
(`192.168.77.1`, netmask `255.255.255.0`), cabled to the board's RJ45. The tool is
[`tools/adc-fft/adc_fft.py`](../../tools/adc-fft/adc_fft.py); `dev.py fft --help` lists its options
(`--rate`, `--fft`, `--no-browser`, `--http-port`, `--host 0.0.0.0` to look from another computer...).

## How it works

| File | What it does |
|---|---|
| [`src/dma.rs`](src/dma.rs) | the ADC's **continuous mode with DMA**: the ADC's own timer triggers every conversion (the 40 MHz crystal divided: exactly regular sampling instants) and the LP AHB GDMA stores the results in a ring of buffers. Written against the registers, following ESP-IDF's `adc_continuous` (`~/.espressif/v6.1/esp-idf`): esp-hal has no continuous ADC for the S31 and doesn't expose that DMA. The chip's limits are 611 Hz to 83 333 Hz. |
| [`src/stream.rs`](src/stream.rs) | the UDP protocol (`start rate=20000 n=256` from the computer every second; sample datagrams back, with the exact rate in the header) and the main loop |
| [`src/board_log.rs`](src/board_log.rs) | the board's log: in colour on the USB console, kept in a short history, and sent to the computer (`LOG1` datagrams) for the page's terminal |
| the Ethernet bring-up | `esp32_rust_workshop_eth::net` (PHY, EMAC, network stack), reused from [`firmware/ethernet`](../ethernet) |

esp-hal's one-shot ADC takes about 57 µs per conversion (10 kS/s at best, with the CPU busy); with the DMA the
CPU only copies finished buffers into datagrams. The page's controls (sample rate, FFT length, window,
averaging, peak hold, reference trace, harmonic markers, waveform trigger...) sit next to the plot they change.

- Measured on the board at 20, 40 and 83.3 kS/s (with the previous console): no datagram lost, no sample lost, no
  DMA stall, and the exact rate (83 333.3 Hz) in the headers. A finger on a wire shows the 50 Hz mains at
  50.0 Hz, so the timing is right to about 0.1 %. Not measured: the level of a real signal in volts.
- The S31's SAR is differential: 0 V reads about 2198 and full scale is 4393, with no calibration, so
  levels are relative (dB below full scale) and the mean is removed before the FFT.
- Above a few kS/s, put an **RC low-pass** on the input (cut-off below half the sample rate), or frequencies
  above it fold back into the spectrum. Keep the input between 0 and 3.3 V.
- A lost datagram or a sample lost on the board empties the FFT window (the spectrum never spans a gap) and
  shows in the counters and in the log.

The datagram format is documented in [`src/stream.rs`](src/stream.rs) and [`src/board_log.rs`](src/board_log.rs).

## Commands from the page's terminal

The terminal under the page's "Board log" has a prompt: what you type goes to the board (UDP, same port as
`start`), the answer comes back in the log. Buttons next to the filters do the same for the common ones.

| command | does |
|---|---|
| `info` | six lines, the node firmware's boot card in short: chip and MAC, build, ADC, network and link, stream (rate, datagrams, losses, DMA stalls), uptime |
| `verbose [on\|off]` | one detail line per second: datagrams, longest burst, longest `send_to`, longest pass of the main loop, ring level, DMA buffers and stalls, log lines sent |
| `reboot` | software reset; the page's keep-alive restarts the stream and re-applies verbose mode |
| `help` | the list |

Verbose mode is built not to disturb the samples: the loop only increments counters and keeps maxima (a few
integer operations and two clock reads per datagram, whether verbose is on or not), and one line is
formatted and sent per second, through the same bounded path as the other log lines (at most 6 per pass,
after the samples' turn). Nothing is written per sample or per datagram. Anyone on the network can send these
commands (there is no authentication: this is a bench tool on a point-to-point cable).
