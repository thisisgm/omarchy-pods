//! The fenced binding to the system `PipeWire` library: one stream source node, proved by its tests against a real server. The library reads `PIPEWIRE_REMOTE` before any property, so the crate connects the socket itself and `Spec::remote` alone names the server.
#![cfg_attr(
    test,
    expect(
        dead_code_pub_in_binary,
        reason = "the tests under tests/ drive the source and read the two messages back"
    )
)]

#[expect(
    unsafe_code,
    dead_code,
    unreachable_pub,
    nonstandard_style,
    reason = "written by `cargo xtask pw-ffi` from the library's headers, whole types and C names as they are"
)]
mod ffi;
pub mod pod;
pub mod source;

pub use source::{Event, Fill, OpenError, Source, Spec};

// The library by its soname, so a build needs no development files; the hosted `test` job builds without them.
#[expect(
    unsafe_code,
    reason = "names the library for the linker; `ffi` holds the declarations"
)]
#[link(name = "libpipewire-0.3.so.0", kind = "dylib", modifiers = "+verbatim")]
unsafe extern "C" {}
