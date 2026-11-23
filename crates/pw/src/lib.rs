//! The binding to the system `PipeWire` library. This part is safe code: the two parameter messages a stream source node sends.
#![cfg_attr(
    test,
    expect(
        dead_code_pub_in_binary,
        reason = "the unit test build calls neither message; the tests under tests/ read both back"
    )
)]

pub mod pod;
