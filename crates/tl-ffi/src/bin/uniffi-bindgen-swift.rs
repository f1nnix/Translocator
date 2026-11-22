//! `uniffi-bindgen-swift`, built from the same uniffi as the library so the
//! generated Swift always matches it. See `macos/build-core.sh`.

fn main() {
    uniffi::uniffi_bindgen_swift()
}
