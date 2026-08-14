use std::ffi::c_void;

pub const EXTENSION_ABI_VERSION: u32 = 1;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct AbiBuffer {
    pub ptr: *mut u8,
    pub len: usize,
}

impl AbiBuffer {
    pub fn from_string(value: String) -> Self {
        Self::from_vec(value.into_bytes())
    }

    pub fn from_vec(mut value: Vec<u8>) -> Self {
        let buffer = Self {
            ptr: value.as_mut_ptr(),
            len: value.len(),
        };
        std::mem::forget(value);
        buffer
    }
}

pub type CompletionCallback = unsafe extern "C" fn(*mut c_void, AbiBuffer, bool);

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ExtensionV1 {
    pub abi_version: u32,
    pub metadata: unsafe extern "C" fn() -> AbiBuffer,
    pub start_call: unsafe extern "C" fn(
        u64,
        *const u8,
        usize,
        *const u8,
        usize,
        CompletionCallback,
        *mut c_void,
    ),
    pub drop_session: unsafe extern "C" fn(u64),
    pub set_cancelled: unsafe extern "C" fn(bool),
    pub free_buffer: unsafe extern "C" fn(AbiBuffer),
}

#[doc(hidden)]
pub unsafe extern "C" fn free_buffer(buffer: AbiBuffer) {
    if !buffer.ptr.is_null() {
        drop(unsafe { Vec::from_raw_parts(buffer.ptr, buffer.len, buffer.len) });
    }
}
