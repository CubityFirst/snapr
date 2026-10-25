//! Starting snapr when you sign in.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
pub use linux::{enabled, refresh, set};
#[cfg(windows)]
pub use windows::{refresh, set};
