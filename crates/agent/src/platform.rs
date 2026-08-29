//! Compile-time selection of the OS adapter.

use filemind_core::OsAdapter;

#[cfg(unix)]
pub fn adapter() -> Box<dyn OsAdapter> {
    Box::new(filemind_adapter_macos::MacAdapter)
}

#[cfg(windows)]
pub fn adapter() -> Box<dyn OsAdapter> {
    Box::new(filemind_adapter_win::WinAdapter)
}
