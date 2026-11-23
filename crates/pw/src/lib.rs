//! The fenced binding to the system `PipeWire` library. So far: its declarations, the link, and the two parameter messages a stream source node sends.
#![cfg_attr(
    test,
    expect(
        dead_code_pub_in_binary,
        reason = "the unit test build calls neither message; the tests under tests/ read both back"
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

// The library by its soname, so a build needs no development files; the hosted `test` job builds without them.
#[expect(
    unsafe_code,
    reason = "names the library for the linker; `ffi` holds the declarations"
)]
#[link(name = "libpipewire-0.3.so.0", kind = "dylib", modifiers = "+verbatim")]
unsafe extern "C" {}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::ptr;

    use super::ffi;

    /// The soname, which is also how the file name of every version of the library starts.
    const SONAME: &str = "libpipewire-0.3.so.0";

    #[test]
    #[expect(
        unsafe_code,
        reason = "one call into the library, so the linker keeps it"
    )]
    fn a_call_into_the_library_loads_it() {
        // SAFETY: `pw_init` takes null for both arguments.
        unsafe { ffi::pw_init(ptr::null_mut(), ptr::null_mut()) };
        let maps = fs::read_to_string("/proc/self/maps").unwrap();
        // Sample input: `7f3a1c000000-7f3a1c00f000 r--p 00000000 08:01 1234 /usr/lib/x86_64-linux-gnu/libpipewire-0.3.so.0.1402.0`.
        let mapped = maps
            .lines()
            .filter_map(|line| line.rsplit('/').next())
            .any(|file| file.starts_with(SONAME));
        assert!(mapped, "no {SONAME} among the mapped files: {maps}");
    }
}
