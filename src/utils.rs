use std::ffi::{CStr, CString};
use std::marker::PhantomData;
use std::ptr;
use std::rc::Rc;

use windows_core::{GUID, Interface};

use crate::win::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize, E_POINTER};
use crate::{Error, Result, WinResult, sys};

/// Query an interface whose IID is provided by its driver rather than its Rust type.
///
/// # Safety
/// `target_iid` must identify `Target` on this object. The returned interface must
/// remain in its initialized COM apartment and obey its threading contract.
pub(crate) unsafe fn cast_decoupled<Target: Interface>(
    interface: &impl Interface,
    target_iid: &GUID,
) -> WinResult<Target> {
    let mut raw = ptr::null_mut();
    unsafe { interface.query(target_iid, &raw mut raw) }.ok()?;
    if raw.is_null() {
        return Err(E_POINTER.into());
    }
    Ok(unsafe { Target::from_raw(raw) })
}

pub(crate) fn parse_string(field: &'static str, bytes: &[u8]) -> Result<CString> {
    CStr::from_bytes_until_nul(bytes)
        .map(CStr::to_owned)
        .map_err(|_| Error::UnterminatedString { field })
}

pub(crate) fn parse_bool(field: &'static str, value: sys::Bool) -> Result<bool> {
    bool::try_from(value).map_err(|_| Error::InvalidBoolean {
        field,
        value: value.0,
    })
}

pub(crate) fn check_code(operation: &'static str, code: sys::ResultCode) -> Result<()> {
    code.ok(())
        .map_err(|code| Error::Driver { operation, code })
}

pub(crate) fn check_future_code(code: sys::ResultCode) -> Result<()> {
    code.future_ok(()).map_err(|code| Error::Driver {
        operation: "future",
        code,
    })
}

/// One successful STA initialization; deliberately neither cloneable nor sendable.
pub(crate) struct COMInit(PhantomData<Rc<()>>);

impl COMInit {
    pub(crate) fn new() -> WinResult<Self> {
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED as _) }.ok()?;
        Ok(Self(PhantomData))
    }
}

impl Drop for COMInit {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}
