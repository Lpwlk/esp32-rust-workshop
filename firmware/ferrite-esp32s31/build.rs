fn main() {
    // The examples are firmware: they need esp-hal's linker script, last.
    println!("cargo:rustc-link-arg-examples=-Tlinkall.x");
}
