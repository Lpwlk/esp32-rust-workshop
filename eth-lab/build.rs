fn main() {
    // linkall.x must be the last linker script.
    println!("cargo:rustc-link-arg=-Tlinkall.x");
    println!("cargo:rerun-if-env-changed=ETH_LAB_HOST");
}
