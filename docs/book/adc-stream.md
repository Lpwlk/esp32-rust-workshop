# The ADC app: a board's analog input on your screen

A third firmware next to the two board-to-board apps. **One** ESP32-S31 board samples an analog pin at up to
83 kS/s and streams the samples over Ethernet (UDP) to a computer, where a web page draws the spectrum, the
waveform and a waterfall, with the board's log in a terminal. It can also play a PWM wave on another pin, to
give the ADC a signal you know.

| | |
|---|---|
| firmware | [`firmware/adc-stream/`](https://github.com/Lpwlk/esp32-rust-workshop/tree/main/firmware/adc-stream) (its own Cargo workspace, like the Ethernet app) |
| computer side | [`tools/adc-fft/adc_fft.py`](https://github.com/Lpwlk/esp32-rust-workshop/tree/main/tools/adc-fft) + `adc_fft.html` (Python, numpy, Plotly) |
| board | `192.168.77.20`, UDP port 7100; the computer takes `192.168.77.1/24` |
| analog input | **GPIO43** (`ADC1_CH1`, pin `43` of header J2), 0 to 3.3 V |
| PWM output | **GPIO20** (`D0` of header J2) |

## Run it

```sh
python3 scripts/dev.py setup                 # once: numpy and plotly go in the .venv
python3 scripts/dev.py --app adc flash a -m  # build, flash node A's board (or b), open its console
python3 scripts/dev.py fft                   # on the computer: the page, http://127.0.0.1:8050
python3 scripts/dev.py fft --demo            # the same page with a simulated board: no hardware at all
```

Give the computer's Ethernet port (a USB-C adapter is fine) the address `192.168.77.1`, netmask
`255.255.255.0`, and cable it to the board's RJ45. Without Rust: `python3 tools/adc-fft/adc_fft.py` from the
`.venv`, or `.venv/bin/python tools/adc-fft/adc_fft.py --demo`.

A phone or tablet on the same Wi-Fi: `dev.py fft --lan` prints the address to type there. Anyone on that network
can then change the settings and reboot the board, so use it on a network you trust.

## How the data travels

```text
board ──UDP──▶ Stream (thread: datagrams → last samples)
                  │
               Analyzer (windowed FFT, averaging, peak hold, filter, trigger)  ◀── POST /api/config
                  │ one JSON frame every 1/fps (20 per second)
               Hub ──▶ GET /events (Server-Sent Events) ──▶ the page draws it with Plotly
```

- The computer sends `start rate=20000 n=256` to the board **every second** (a keep-alive); the board streams
  datagrams of raw samples back to the sender and goes idle when the keep-alive stops. The exact rate
  is in each datagram's header, so the frequency axis is right even when the ADC clock can't divide exactly.
- The ADC runs in **continuous mode with DMA**: its own timer triggers each conversion (exactly regular
  instants) and the DMA fills a ring of buffers; the CPU only copies finished buffers into datagrams. esp-hal's
  one-shot ADC takes about 57 µs per conversion (10 kS/s at best), hence the register-level driver in
  `firmware/adc-stream/src/dma.rs`. The chip's limits are **611 Hz to 83 333 Hz**.
- A lost datagram, or a sample lost on the board, **empties the FFT window**: a spectrum never spans a gap. It
  shows in the counters and in the log.
- The S31's SAR is **differential**: 0 V reads about 2198 and full scale is 4393, with no calibration, so
  levels are relative (dB below full scale), the mean is removed before the FFT, and "mV" in the page is an
  estimate you can set.
- Above a few kS/s, put an **RC low-pass** on the input (cut-off below half the sample rate), or frequencies above
  it fold back into the spectrum. Keep the input between 0 and 3.3 V.

The datagram formats are documented in `src/stream.rs` (`ADC1` samples, `RPL1` answers) and `src/board_log.rs`
(`LOG1` log lines). The Wireshark plugin decodes all three: [Tools and workflows](tools.md#wireshark).

## The page

Every card has a title. The **header** has two rows:

- the chips: `Demo` when the board is simulated, `Streaming` with the rate, `Board` with the pin and address, `PWM`
  while the generator runs, the **colour theme** and the light/dark **mode**;
- a strip of live numbers, each with a dot (green, orange, red) so a glance says whether the link is healthy: sample
  rate, datagrams per second (orange when far from what the rate predicts), throughput on the wire, lost datagrams,
  age of the last datagram, the strongest line, and whether your settings are saved.

Under it, the **Status** box (click its title to fold it, the page remembers) gives the details in three columns:
the *network link* (link up or no data, the board's address, this computer and how many pages are open, datagrams
received against expected, throughput, losses), the *board* (analog pin, exact sampling rate against the one asked,
samples per datagram, verbose log, PWM generator) and *this tool* (live or paused, FFT, window, averaging, filter,
trigger, frames per second, uptime, the settings file and when it was written).

**Colour theme.** The *Colours* selector in the header is global: it sets the traces, the waterfall's colour scale
and the **accent** of buttons, sliders, selected buttons and focus outlines at once (Blue + orange, Oscilloscope,
Amber, Cyan + magenta, Mint + peach, Mint + rose, Sky + orchid, Colour-blind safe; the pastel ones have a deeper
variant on a light page). Picking a colour in the Spectrum or Waveform card makes it *Custom*. *Mode* is light,
dark or auto (follows your system).

| Card | What it does |
|---|---|
| **Acquisition** | **listen** to the signal (play, volume), sample rate, FFT length (1024 to 32768), window (Hann, Hamming, Blackman, flat-top, rectangular), averaging, peak hold, which plots to show (including *Harmonics* and *Spectrum + harmonics*), pause, **reset all**, CSV export of the spectrum or the samples |
| **Measurements** | peak frequency and level, AC rms, DC level, SNR and THD (estimates), noise floor, and the network (datagrams per second, losses) |
| **Digital filter** | low-pass, high-pass, band-pass, band-stop (FIR, four steepnesses), applied to everything shown, with the unfiltered signal underneath for comparison |
| **Signal generator** | *demo only*: up to eight simulated waveforms you **add** (*+ Add waveform*) and **remove** (×) one by one (sine, square, PWM, sawtooth, triangle, sweep, noise), hum, noise, DC, presets |
| **PWM generator** | the board's real generator on GPIO20, see below |
| **Harmonics** | the strongest line and its multiples as bars (dB below the line, or below full scale); harmonics above half the sample rate are **hatched**, drawn where they fold back to; the noise floor; an ideal square, triangle or sawtooth as diamonds to compare with; a summary (THD, even/odd) |
| **Spectrum** | dB or linear, log or linear frequency, peak hold, a reference trace, A/B cursors, harmonic markers (strongest line, 50 or 60 Hz mains), autoscale |
| **Waveform** | the last milliseconds of samples, with an oscilloscope-style **trigger** (edge, tone or pattern lock; slope, level, position; auto or normal), averaging, min/max envelope with a sliding window, mains hum removal, units in codes, % or mV |
| **Strongest lines**, **Code histogram** | the peaks of the spectrum, and how the raw ADC codes are distributed (clipping, stuck bits) |
| **Waterfall** | the spectrum over time, with a colour scale per theme |
| **Board log** | what the board prints, filterable, and a prompt for its [commands](#board-commands) |

### Fold what you don't need

Every card folds to its title: click the chevron or the title (the *Status* box already did). In *Measurements* each
tile folds **on its own**, and the title folds or unfolds them all. What is folded is kept with the other view settings, and a plot
is redrawn at its size when you unfold it.

### Listen to the signal

The *Acquisition* card has a **Listen** group: **Play / Pause**, **Mute**, a **volume** slider (shown in dB: off, then −50 to
+24 dB), **Auto level** and **Filtered**, with a level meter and the line `60 kS/s → 48 kHz · 300 ms ahead`. The sound plays in
your **browser**, on the computer that shows the page, so it is the same on macOS, Windows and Linux (Chrome, Edge,
Firefox, Safari); nothing to install. It starts only when you click Play (browsers refuse sound on their own).

- the page asks the tool for the raw samples (`GET /audio`, in the order they arrive, with the board's exact rate), removes the DC level
  (a high-pass at 20 Hz: the ADC's offset is not a sound), resamples them to the sound card's rate with a windowed sinc, with a
  low-pass first when the board is faster (what is above the audible band is removed instead of folding into it),
  and plays them back to back. The sound is the **raw signal**, unless you press **Filtered**: then the tool runs the page's digital filter (the card
  *Digital filter*, as it is set at each moment) on the stream and the page plays its output (`GET /audio?filtered=1`, float32 samples). With
  the filter off, Filtered changes nothing (the line says *filter off*). The hum removal is not applied to the sound;
- about 0.15 to 0.35 s of sound is kept ahead so a late datagram doesn't cut it. The board's clock and the sound card's differ a
  little, so the playback speeds up or slows down by 0.4 % at most to hold that delay (no click, a few cents of pitch);
  `gap` in the line counts the times the sound ran dry;
- a **limiter** squeezes loud peaks instead of letting them clip, and the volume starts at +2 dB. **Auto level** raises a weak
  signal up to 400 times and follows it (noise gets loud too: turn the volume down first). The volume, mute and auto level are kept like
  the other view settings.
- A rate below the audible range (a 100 Hz signal sampled at 1 kS/s) is heard as it is: low. A pure 50 Hz hum is
  mostly felt on a laptop speaker, which doesn't reproduce it; use headphones.

### Your settings are kept

Everything you choose is written to a file, so **rebuilding, restarting, changing the port or opening the page in
another browser finds it again**: the sample rate, FFT, window, averaging, filter, trigger, hold... (the
analyzer's settings), the page's view settings (plots shown, scales, cursors' units, colours, the colour
theme, light or dark, log filters, the PWM values you typed), and in the demo the simulated signal.

- The file is `~/.config/adc-fft/settings.json` (under `$XDG_CONFIG_HOME` when it is set). It is plain JSON; you
  can read it, copy it to another computer, or delete it.
- It is written half a second after the last change (a slider sends many) and at exit, whole or not at all.
- **What is not kept**: the pause (a page that opens paused looks broken), and the board's own state: the PWM
  generator is off after every reboot.
- A command-line option wins over the saved value for that run (`--rate 40000`) and is saved when you change
  something. `--settings FILE` uses another file (one per bench, say), `--no-save` runs from the defaults and
  keeps nothing, `--reset-settings` forgets the file and starts from the defaults.
- The **Reset all** button (card *Acquisition*, group *Settings*) does the same from the page: everything is back
  to the defaults and the file is forgotten.
- A damaged file, or one written by another version, is ignored with a warning in the board log: the tool starts
  from the defaults and replaces the file at the next change.

### Options of the tool

`python3 scripts/dev.py fft --help` (the options go to the tool):

| Option | |
|---|---|
| `--board`, `--port`, `--count` | the board's address (`192.168.77.20`), its UDP port (7100), samples per datagram (256) |
| `--rate`, `--fft`, `--avg`, `--scope-ms` | initial values, instead of the saved ones |
| `--http-port`, `--host`, `--lan`, `--fps` | where the page is served (8050 on 127.0.0.1), shown to the network, frames per second |
| `--settings`, `--no-save`, `--reset-settings` | the settings file, see above |
| `--demo` | a simulated board on this computer |
| `--no-browser` | don't open the page |
| `--selftest` | receiver, FFT, web server, saved settings and PWM commands against a simulated board, no browser (CI runs it) |

## Board commands

The terminal under *Board log* sends what you type to the board (UDP, same port as the stream); the answer
comes back in the log. Buttons next to the filters do the same for the common ones.

| Command | Does |
|---|---|
| `info` | chip and MAC, build, ADC, network and link, stream (rate, datagrams, losses, DMA stalls), uptime |
| `verbose [on\|off]` | one detail line per second: datagrams, timings, ring level, DMA buffers. It doesn't slow the stream, and is re-applied after a reboot |
| `pwm [freq=HZ] [duty=%] [on\|off]` | the PWM generator; `pwm 1000 25` is 1 kHz at 25 %, `pwm` alone tells its state |
| `reboot` | software reset; the stream resumes by itself |
| `help` | the list |

There is no authentication: this is a bench tool on a point-to-point cable.

## PWM generator: a signal you know

The **PWM generator** card (or `pwm` in the terminal) makes a square wave on **GPIO20**. Wire it to `43` to read
it back in the spectrum and the waveform, through an RC low-pass to see the duty cycle as a level, or directly to
see the edges and the harmonics.

- 0.2 Hz to 1 MHz, any duty from 0 to 100 %. The period is a whole number of ticks of 12.5 ns, so the frequency is
  exact to 6 ns of period (0.0005 % at 1 kHz). Changing a value restarts the wave; off, the pin idles low.
- Above half the sample rate the page tells you where the wave **folds back** to.
- It is the RMT transmitter in loop mode: the channel RAM holds one period as pulse codes and the hardware replays
  it with no CPU (esp-hal has no LEDC or MCPWM for the S31). The arithmetic is in `src/pwm_plan.rs`, tested on
  your computer.
- GPIO20 also drives breadboard LED 1 of the node apps, if it is wired: a dimmer for the duty cycle at a few
  hundred Hz.
- Not measured on a board yet: reading it back through a wire is the test.

## Tests

| What | Command |
|---|---|
| firmware builds and lints | `cd firmware/adc-stream && cargo build --release && cargo clippy --release -- -D warnings` |
| PWM arithmetic (no hardware) | `cd firmware/adc-stream && rustc --edition 2024 --test src/pwm_plan.rs -o /tmp/pwm_plan_test && /tmp/pwm_plan_test` |
| computer side: receiver, FFT, web server, saved settings, board commands | `python3 tools/adc-fft/adc_fft.py --selftest` |
| formatting | `cd firmware/adc-stream && cargo fmt`, and `python3 scripts/dev.py format` for the Python |
| everything CI runs | `python3 scripts/dev.py check` |

## If nothing shows up

| Symptom | Check |
|---|---|
| "No data from the board yet" | the board is flashed with `--app adc`, the cable is in, the computer's interface is `192.168.77.x`; `dev.py monitor a` shows the board's own log |
| the status chip says *waiting* | no datagram for 1.5 s: the cable, or another program holding the board's UDP port |
| a spectrum full of lines you didn't send | aliasing: add the RC low-pass, or raise the sample rate |
| the page forgets its settings | `--no-save` is on, or the settings file is not writable (the board log says so) |
