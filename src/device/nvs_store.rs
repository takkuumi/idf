//! Shared blob replacement without esp-idf-svc 0.53's delete-before-set.
//!
//! Call only while holding the existing device NVS mutex. The IDF setter
//! compares identical blobs and writes a changed blob before invalidating its
//! previous version. Keep the explicit commit at the same per-key boundary as
//! EspNvs::set_blob; this is not a multi-key transaction or a deferred cache.

use esp_idf_svc::handle::RawHandle;
use esp_idf_svc::nvs::EspDefaultNvs;
use esp_idf_sys::{ESP_ERR_NVS_INVALID_NAME, ESP_ERR_NVS_KEY_TOO_LONG, EspError};

const KEY_BYTES: usize = 16;

fn key_bytes(name: &str) -> Result<[u8; KEY_BYTES], EspError> {
    if name.is_empty() || name.as_bytes().contains(&0) {
        return Err(EspError::from_infallible::<ESP_ERR_NVS_INVALID_NAME>());
    }
    if name.len() >= KEY_BYTES {
        return Err(EspError::from_infallible::<ESP_ERR_NVS_KEY_TOO_LONG>());
    }
    let mut key = [0; KEY_BYTES];
    key[..name.len()].copy_from_slice(name.as_bytes());
    Ok(key)
}

pub(crate) fn set_blob(nvs: &EspDefaultNvs, name: &str, value: &[u8]) -> Result<(), EspError> {
    let key = key_bytes(name)?;
    // SAFETY: the NVS handle is borrowed for the entire synchronous call;
    // key is validated/NUL-terminated and both slices remain live. No erase
    // precedes the IDF replacement, so a failed setter is never followed by
    // an acknowledgement. Commit errors also propagate to existing retries.
    EspError::convert(unsafe {
        esp_idf_sys::nvs_set_blob(
            nvs.handle(),
            key.as_ptr().cast(),
            value.as_ptr().cast(),
            value.len(),
        )
    })?;
    EspError::convert(unsafe { esp_idf_sys::nvs_commit(nvs.handle()) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_key_rejects_empty_nul_and_overlong_names() {
        assert_eq!(key_bytes("").unwrap_err().code(), ESP_ERR_NVS_INVALID_NAME);
        assert_eq!(
            key_bytes("sys\0cfg").unwrap_err().code(),
            ESP_ERR_NVS_INVALID_NAME
        );
        assert_eq!(
            key_bytes("0123456789abcdef").unwrap_err().code(),
            ESP_ERR_NVS_KEY_TOO_LONG
        );
    }

    #[test]
    fn test_key_accepts_fifteen_bytes_and_terminates_without_allocation() {
        assert_eq!(&key_bytes("0123456789abcde").unwrap(), b"0123456789abcde\0");
        assert_eq!(&key_bytes("sys_cfg").unwrap()[..8], b"sys_cfg\0");
    }
}
