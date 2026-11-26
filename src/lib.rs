//! GString Captions — native OBS Studio audio filter plugin.
//!
//! Exports the standard OBS module ABI (`obs_module_set_pointer`,
//! `obs_module_ver`, `obs_module_load`, `obs_module_unload`, metadata) and
//! registers the `GString Captions` audio filter.

pub mod audio;
pub mod config;
mod filter;
pub mod obs_abi;
pub mod worker;

use std::ffi::c_char;
use std::panic::catch_unwind;
use std::sync::atomic::{AtomicPtr, Ordering};

use obs_abi::{LIBOBS_API_VER, LOG_ERROR, LOG_INFO, init_api, log, obs_module_t};

static OBS_MODULE_POINTER: AtomicPtr<obs_module_t> = AtomicPtr::new(std::ptr::null_mut());

/// # Safety
/// Called by OBS Studio with a valid module pointer during plugin load.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obs_module_set_pointer(module: *mut obs_module_t) {
    OBS_MODULE_POINTER.store(module, Ordering::SeqCst);
}

#[unsafe(no_mangle)]
pub extern "C" fn obs_module_ver() -> u32 {
    LIBOBS_API_VER
}

#[unsafe(no_mangle)]
pub extern "C" fn obs_module_name() -> *const c_char {
    c"GString Captions".as_ptr()
}

#[unsafe(no_mangle)]
pub extern "C" fn obs_module_description() -> *const c_char {
    c"Captures OBS audio directly into GString live captions without a browser capture tab."
        .as_ptr()
}

#[unsafe(no_mangle)]
pub extern "C" fn obs_module_author() -> *const c_char {
    c"GString".as_ptr()
}

#[unsafe(no_mangle)]
pub extern "C" fn obs_module_load() -> bool {
    catch_unwind(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
        match init_api() {
            Ok(obs) => {
                filter::register_frontend_hooks(obs);
                filter::register_filter(obs);
                log(LOG_INFO, concat!("loaded v", env!("CARGO_PKG_VERSION")));
                true
            }
            Err(err) => {
                log(LOG_ERROR, &format!("failed to load: {err}"));
                false
            }
        }
    })
    .unwrap_or(false)
}

#[unsafe(no_mangle)]
pub extern "C" fn obs_module_unload() {
    let _ = catch_unwind(|| {
        filter::unregister_frontend_hooks();
    });
}
