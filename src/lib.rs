//! Windows x64 ASIO bindings with one control owner and explicit registration lifetime.
//! See [`driver::Owner`] for the safety contract of registered callbacks.

pub mod driver;
pub mod dto;
pub mod error;
pub mod future;
mod utils;
pub mod win;

pub use azo_sys as sys;
pub use error::{Error, Result};
pub use win::HWND;
pub use windows_core;

pub type WinResult<T> = windows_core::Result<T>;

#[cfg(test)]
mod driver_tests;
