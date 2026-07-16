//! PyO3 bindings for sonagram.
//!
//! This crate is the thin `_sonagram` extension module. Pure-Rust logic lives
//! in the `sonagram` core crate (PyO3-free by design); this layer only maps it
//! to Python. The API surface (`scan`, `export_m3u`, rescan) lands in P8.

use pyo3::prelude::*;

/// The compiled `_sonagram` module. Imported by `python/sonagram/__init__.py`
/// as `from sonagram._sonagram import *`.
#[pymodule]
fn _sonagram(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Version — sourced from this crate's Cargo.toml via env! at compile time,
    // kept in lockstep with the core crate and pyproject.toml.
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
