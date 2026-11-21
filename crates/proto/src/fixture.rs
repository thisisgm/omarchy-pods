//! Test only: reads the hex text the packet tests write their documented bytes in.

// Sample input: "04 00 58", hex bytes separated by one space.
pub(crate) fn hex(text: &str) -> Vec<u8> {
    text.split(' ')
        .map(|byte| u8::from_str_radix(byte, 16).unwrap())
        .collect()
}
