# Flint compatibility check

`steel_compat_lib.rs` decodes a READY and a message captured from a running `steeld` with the
`discord-protocol` crate from Flint. It guards against changes in Steel's payloads that the Flint UI cannot parse.

To run it, copy the file into a crate inside a Flint checkout, next to `crates/discord-protocol`:

    crates/steel-compat/Cargo.toml   # depends on discord-protocol (path) and serde_json
    crates/steel-compat/src/lib.rs   # this file

Then `cargo +1.99.1 test -p steel-compat` (Flint pins toolchain 1.99.1).
