# adc-fft: the live page for the ADC app

`adc_fft.py` (Python, numpy) receives the samples streamed by [`firmware/adc-stream`](../../firmware/adc-stream),
computes the spectrum, and serves `adc_fft.html` (Plotly) at <http://127.0.0.1:8050>.

```sh
python3 scripts/dev.py setup        # once: numpy and plotly go in the .venv
python3 scripts/dev.py fft          # the page, with the board at 192.168.77.20
python3 scripts/dev.py fft --demo   # a simulated board on this computer
python3 tools/adc-fft/adc_fft.py --selftest   # tests the whole chain, no browser, no board (CI runs it)
```

Everything the page does, the board's commands, the PWM generator and the options:
[the ADC app page](../../docs/book/adc-stream.md).

Play the signal as sound with the **Listen** group of the card *Acquisition* (play/pause, mute, volume, auto level, and
**Filtered** for the output of the digital filter): the browser plays it, so it works on macOS and Windows alike
(`GET /audio` feeds the page the samples, `?filtered=1` after the filter). Every card folds to its title.

## Your settings are kept

The sample rate, FFT, window, averaging, filter, trigger and hold, the page's view settings (plots shown, scales,
colour theme, light/dark mode, log filters, harmonics options, whether the status box is open, PWM values) and, in the demo, the simulated signal are written to
`~/.config/adc-fft/settings.json` (`$XDG_CONFIG_HOME/adc-fft/settings.json` when set), half a second after the last
change and at exit. The next run, on any port or in any browser, starts from them.

| | |
|---|---|
| `--settings FILE` | another file (one per bench) |
| `--no-save` | start from the defaults and keep nothing |
| `--reset-settings` | forget the file, then start |
| `--rate`, `--fft`, `--avg`, `--scope-ms` | win over the saved value for this run |
| *Reset all* (page, card *Acquisition*) | back to the defaults, file forgotten |

The pause is never kept, and the board's own state (the PWM generator is off after a reboot) lives on the board.

## How the page and the server share it

- the analyzer's settings are validated by `Analyzer.configure` (the same code for the page's controls and for the
  file), saved by `POST /api/config`;
- the page's `ui` object and the theme are saved by `POST /api/prefs` (400 ms after the last change) and written
  into the page by the server when it is served (`SAVED` in `adc_fft.html`): the page draws with them at once,
  and only keys it knows, of the type it expects, are taken;
- `POST /api/reset` puts the analyzer back to the defaults and deletes the file.
