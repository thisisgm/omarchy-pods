//! The source properties sent at open and the card ID updates sent to the live stream.

use std::ffi::{CStr, CString};
use std::io;

use super::{Locked, OpenError, Source, Spec};
use crate::ffi;

const PROPERTIES: usize = 6;
#[expect(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "`TryFrom` is not const, and the count is six"
)]
pub(super) const N_PROPERTIES: u32 = PROPERTIES as u32;

/// The node's properties as the C strings the library copies; the name comes first.
pub(super) struct Properties(pub(super) [(&'static CStr, CString); PROPERTIES]);

impl Properties {
    pub(super) fn new(spec: &Spec<'_>) -> Result<Self, OpenError> {
        let owned = |key: &'static CStr, text: &str| {
            let text = CString::new(text).map_err(|error| OpenError::Library {
                call: "pw_properties_new_dict",
                error: io::Error::new(io::ErrorKind::InvalidInput, format!("{key:?}: {error}")),
            })?;
            Ok((key, text))
        };
        Ok(Self([
            owned(c"node.name", spec.name)?,
            (c"media.class", c"Audio/Source".to_owned()),
            owned(c"node.description", spec.description)?,
            (c"node.virtual", c"false".to_owned()),
            owned(c"priority.session", &spec.priority.to_string())?,
            (c"node.slow-resume", c"true".to_owned()),
        ]))
    }
}

impl Source {
    /// Updates the Bluetooth card ID without replacing the node.
    #[expect(unsafe_code, reason = "updates the stream under its loop's lock")]
    pub fn set_device_id(&self, id: Option<u32>) -> io::Result<()> {
        let value = id.map(|id| id.to_string()).map(CString::new).transpose()?;
        let item = ffi::spa_dict_item {
            key: c"omarchypods.device.id".as_ptr(),
            // The server merges node properties, so omission would keep the last card ID.
            value: value.as_deref().unwrap_or(c"").as_ptr(),
        };
        let dict = ffi::spa_dict {
            flags: 0,
            n_items: 1,
            items: &raw const item,
        };
        let _lock = Locked::take(self.thread_loop.0);
        // SAFETY: the lock is held, and the live stream copies the dictionary before returning.
        let result =
            unsafe { ffi::pw_stream_update_properties(self.stream.raw.as_ptr(), &raw const dict) };
        if result < 0 {
            return Err(io::Error::from_raw_os_error(result.saturating_neg()));
        }
        Ok(())
    }
}
