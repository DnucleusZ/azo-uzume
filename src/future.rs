use crate::sys::*;

/// This has nothing to do with [`std::future::Future`], ASIO just unfortunately happens to have a concept of the same name.
/// In their defense, ASIO pre-dates widespread adoption of the async concurrency model.
///
/// Only the I/O format operations below are available through the safe API.
/// The trait is sealed so a caller cannot pair a selector with incompatible storage:
///
/// ```compile_fail
/// use azo::{future::AsioFuture, sys::FutureSelector};
/// struct WrongStorage;
/// impl AsioFuture for WrongStorage {
///     const SELECTOR: FutureSelector = FutureSelector::GET_IO_FORMAT;
///     const MUTATES_CONFIGURATION: bool = false;
///     type Param = ();
/// }
/// ```
///
/// A supported operation also requires its fixed parameter type:
///
/// ```compile_fail
/// use azo::future::{AsioFuture, GetIoFormat};
/// fn request<T: AsioFuture>(_: &mut T::Param) {}
/// request::<GetIoFormat>(&mut ());
/// ```
#[expect(
    clippy::module_name_repetitions,
    reason = "prefix added because std's Future is in the prelude"
)]
pub trait AsioFuture: sealed::Sealed {
    const SELECTOR: FutureSelector;
    /// Whether the operation changes the driver's configuration.
    /// The owner must check its configuration state before issuing such a call.
    const MUTATES_CONFIGURATION: bool;
    type Param;
}

mod sealed {
    pub trait Sealed {}
}

macro_rules! Impl {
	($($name:ident, $selector:ident, $mutates:literal),+) => {
		$(
			#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
			pub struct $name;
			impl sealed::Sealed for $name {}

			impl AsioFuture for $name {
				const SELECTOR: FutureSelector = FutureSelector::$selector;
				const MUTATES_CONFIGURATION: bool = $mutates;
				type Param = IoFormat;
			}
		)+
	}
}

Impl!(
    GetIoFormat,
    GET_IO_FORMAT,
    false,
    CanDoIoFormat,
    CAN_DO_IO_FORMAT,
    false,
    SetIoFormat,
    SET_IO_FORMAT,
    true
);
