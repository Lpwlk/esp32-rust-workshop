# API documentation (rustdoc)

Generated from the code comments by `cargo doc` and published with this site on
every push to `main`:

- [`workshop_core`](api/core/workshop_core/index.html): protocol, configuration, CLI,
  debouncing, LED logic, color math. Every example on these pages is a test.
- [`esp32_rust_workshop`](api/firmware/esp32_rust_workshop/index.html): the firmware
  (hardware glue, main loop).

Locally:

```sh
cargo doc -p workshop-core --target host-tuple --open
cargo doc -p esp32-rust-workshop --no-deps --open
```

!!! note
    The `api/` links only work on the published site: CI copies the `cargo doc`
    output there after building the pages.
