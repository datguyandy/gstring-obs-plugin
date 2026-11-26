//! Minimal, ABI-exact runtime bindings to `libobs` and `obs-frontend-api`.
//!
//! Symbols are resolved dynamically from the host OBS process (`obs.dll` /
//! `obs-frontend-api.dll` on Windows, `RTLD_DEFAULT` on macOS/Linux) so the
//! plugin compiles on any machine without downloading the OBS SDK and ships as
//! a single self-contained binary with zero extra runtime DLLs.
//!
//! Every struct layout and enum discriminant matches `obsproject/obs-studio`
//! 31.0.0 (`obs_register_source_s` takes `size_of::<ObsSourceInfo>()` so OBS
//! 30, 31, 32, 33+ all remain binary-compatible).

#![allow(non_camel_case_types, dead_code)]

use std::ffi::{CStr, CString, c_char, c_double, c_float, c_int, c_void};
use std::sync::OnceLock;

/// `MAKE_SEMANTIC_VERSION(30, 0, 0)` — compatible with OBS Studio 30, 31, 32, 33+.
pub const LIBOBS_API_VER: u32 = 30 << 24;

pub const MAX_AV_PLANES: usize = 8;

pub const LOG_ERROR: c_int = 100;
pub const LOG_WARNING: c_int = 200;
pub const LOG_INFO: c_int = 300;
pub const LOG_DEBUG: c_int = 400;

pub const OBS_SOURCE_AUDIO: u32 = 1 << 1;

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum obs_source_type {
    OBS_SOURCE_TYPE_INPUT = 0,
    OBS_SOURCE_TYPE_FILTER = 1,
    OBS_SOURCE_TYPE_TRANSITION = 2,
    OBS_SOURCE_TYPE_SCENE = 3,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum obs_text_type {
    OBS_TEXT_DEFAULT = 0,
    OBS_TEXT_PASSWORD = 1,
    OBS_TEXT_MULTILINE = 2,
    OBS_TEXT_INFO = 3,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum obs_text_info_type {
    OBS_TEXT_INFO_NORMAL = 0,
    OBS_TEXT_INFO_WARNING = 1,
    OBS_TEXT_INFO_ERROR = 2,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum obs_combo_type {
    OBS_COMBO_TYPE_INVALID = 0,
    OBS_COMBO_TYPE_EDITABLE = 1,
    OBS_COMBO_TYPE_LIST = 2,
    OBS_COMBO_TYPE_RADIO = 3,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum obs_combo_format {
    OBS_COMBO_FORMAT_INVALID = 0,
    OBS_COMBO_FORMAT_INT = 1,
    OBS_COMBO_FORMAT_FLOAT = 2,
    OBS_COMBO_FORMAT_STRING = 3,
    OBS_COMBO_FORMAT_BOOL = 4,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum obs_frontend_event {
    OBS_FRONTEND_EVENT_STREAMING_STARTING = 0,
    OBS_FRONTEND_EVENT_STREAMING_STARTED = 1,
    OBS_FRONTEND_EVENT_STREAMING_STOPPING = 2,
    OBS_FRONTEND_EVENT_STREAMING_STOPPED = 3,
    OBS_FRONTEND_EVENT_RECORDING_STARTING = 4,
    OBS_FRONTEND_EVENT_RECORDING_STARTED = 5,
    OBS_FRONTEND_EVENT_RECORDING_STOPPING = 6,
    OBS_FRONTEND_EVENT_RECORDING_STOPPED = 7,
    OBS_FRONTEND_EVENT_SCENE_CHANGED = 8,
    OBS_FRONTEND_EVENT_SCENE_LIST_CHANGED = 9,
    OBS_FRONTEND_EVENT_TRANSITION_CHANGED = 10,
    OBS_FRONTEND_EVENT_TRANSITION_STOPPED = 11,
    OBS_FRONTEND_EVENT_TRANSITION_LIST_CHANGED = 12,
    OBS_FRONTEND_EVENT_SCENE_COLLECTION_CHANGED = 13,
    OBS_FRONTEND_EVENT_SCENE_COLLECTION_LIST_CHANGED = 14,
    OBS_FRONTEND_EVENT_PROFILE_CHANGED = 15,
    OBS_FRONTEND_EVENT_PROFILE_LIST_CHANGED = 16,
    OBS_FRONTEND_EVENT_EXIT = 17,
}

#[repr(C)]
pub struct obs_module_t {
    _private: [u8; 0],
}

#[repr(C)]
pub struct obs_source_t {
    _private: [u8; 0],
}

#[repr(C)]
pub struct obs_data_t {
    _private: [u8; 0],
}

#[repr(C)]
pub struct obs_properties_t {
    _private: [u8; 0],
}

#[repr(C)]
pub struct obs_property_t {
    _private: [u8; 0],
}

#[repr(C)]
pub struct audio_t {
    _private: [u8; 0],
}

#[repr(C)]
pub struct obs_audio_data {
    pub data: [*mut u8; MAX_AV_PLANES],
    pub frames: u32,
    pub timestamp: u64,
}

pub type obs_property_clicked_t = Option<
    unsafe extern "C" fn(
        props: *mut obs_properties_t,
        property: *mut obs_property_t,
        data: *mut c_void,
    ) -> bool,
>;

pub type obs_frontend_event_cb =
    Option<unsafe extern "C" fn(event: obs_frontend_event, private_data: *mut c_void)>;

/// Exact layout of `struct obs_source_info` from OBS Studio 31.0.0.
///
/// Unused callback slots are typed as `Option<unsafe extern "C" fn()>` so the
/// struct layout is pointer-exact while staying concise.
#[repr(C)]
pub struct obs_source_info {
    pub id: *const c_char,
    pub r#type: obs_source_type,
    pub output_flags: u32,
    pub get_name: Option<unsafe extern "C" fn(type_data: *mut c_void) -> *const c_char>,
    pub create: Option<
        unsafe extern "C" fn(settings: *mut obs_data_t, source: *mut obs_source_t) -> *mut c_void,
    >,
    pub destroy: Option<unsafe extern "C" fn(data: *mut c_void)>,
    pub get_width: Option<unsafe extern "C" fn(data: *mut c_void) -> u32>,
    pub get_height: Option<unsafe extern "C" fn(data: *mut c_void) -> u32>,
    pub get_defaults: Option<unsafe extern "C" fn(settings: *mut obs_data_t)>,
    pub get_properties: Option<unsafe extern "C" fn(data: *mut c_void) -> *mut obs_properties_t>,
    pub update: Option<unsafe extern "C" fn(data: *mut c_void, settings: *mut obs_data_t)>,
    pub activate: Option<unsafe extern "C" fn(data: *mut c_void)>,
    pub deactivate: Option<unsafe extern "C" fn(data: *mut c_void)>,
    pub show: Option<unsafe extern "C" fn(data: *mut c_void)>,
    pub hide: Option<unsafe extern "C" fn(data: *mut c_void)>,
    pub video_tick: Option<unsafe extern "C" fn(data: *mut c_void, seconds: c_float)>,
    pub video_render: Option<unsafe extern "C" fn(data: *mut c_void, effect: *mut c_void)>,
    pub filter_video:
        Option<unsafe extern "C" fn(data: *mut c_void, frame: *mut c_void) -> *mut c_void>,
    pub filter_audio: Option<
        unsafe extern "C" fn(data: *mut c_void, audio: *mut obs_audio_data) -> *mut obs_audio_data,
    >,
    pub enum_active_sources: Option<unsafe extern "C" fn()>,
    pub save: Option<unsafe extern "C" fn(data: *mut c_void, settings: *mut obs_data_t)>,
    pub load: Option<unsafe extern "C" fn(data: *mut c_void, settings: *mut obs_data_t)>,
    pub mouse_click: Option<unsafe extern "C" fn()>,
    pub mouse_move: Option<unsafe extern "C" fn()>,
    pub mouse_wheel: Option<unsafe extern "C" fn()>,
    pub focus: Option<unsafe extern "C" fn()>,
    pub key_click: Option<unsafe extern "C" fn()>,
    pub filter_remove: Option<unsafe extern "C" fn(data: *mut c_void, source: *mut obs_source_t)>,
    pub type_data: *mut c_void,
    pub free_type_data: Option<unsafe extern "C" fn(type_data: *mut c_void)>,
    pub audio_render: Option<unsafe extern "C" fn()>,
    pub enum_all_sources: Option<unsafe extern "C" fn()>,
    pub transition_start: Option<unsafe extern "C" fn(data: *mut c_void)>,
    pub transition_stop: Option<unsafe extern "C" fn(data: *mut c_void)>,
    pub get_defaults2:
        Option<unsafe extern "C" fn(type_data: *mut c_void, settings: *mut obs_data_t)>,
    pub get_properties2: Option<
        unsafe extern "C" fn(data: *mut c_void, type_data: *mut c_void) -> *mut obs_properties_t,
    >,
    pub audio_mix: Option<unsafe extern "C" fn()>,
    pub icon_type: c_int,
    pub media_play_pause: Option<unsafe extern "C" fn()>,
    pub media_restart: Option<unsafe extern "C" fn()>,
    pub media_stop: Option<unsafe extern "C" fn()>,
    pub media_next: Option<unsafe extern "C" fn()>,
    pub media_previous: Option<unsafe extern "C" fn()>,
    pub media_get_duration: Option<unsafe extern "C" fn()>,
    pub media_get_time: Option<unsafe extern "C" fn()>,
    pub media_set_time: Option<unsafe extern "C" fn()>,
    pub media_get_state: Option<unsafe extern "C" fn()>,
    pub version: u32,
    pub unversioned_id: *const c_char,
    pub missing_files: Option<unsafe extern "C" fn()>,
    pub video_get_color_space: Option<unsafe extern "C" fn()>,
    pub filter_add: Option<unsafe extern "C" fn(data: *mut c_void, source: *mut obs_source_t)>,
}

impl obs_source_info {
    pub fn zeroed() -> Self {
        // SAFETY: All fields are pointers, function pointers, ints, or enums
        // where 0 is a valid bit pattern (None / null / 0).
        unsafe { std::mem::zeroed() }
    }
}

pub struct ObsFrontendApi {
    pub add_event_callback:
        unsafe extern "C" fn(callback: obs_frontend_event_cb, private_data: *mut c_void),
    pub remove_event_callback:
        unsafe extern "C" fn(callback: obs_frontend_event_cb, private_data: *mut c_void),
    pub streaming_active: unsafe extern "C" fn() -> bool,
    pub recording_active: unsafe extern "C" fn() -> bool,
}

pub struct ObsApi {
    pub blog: unsafe extern "C" fn(log_level: c_int, format: *const c_char, ...),
    pub register_source_s: unsafe extern "C" fn(info: *const obs_source_info, size: usize),
    pub get_audio: unsafe extern "C" fn() -> *mut audio_t,
    pub audio_output_get_sample_rate: unsafe extern "C" fn(audio: *const audio_t) -> u32,
    pub audio_output_get_channels: unsafe extern "C" fn(audio: *const audio_t) -> usize,
    pub filter_get_parent: unsafe extern "C" fn(filter: *const obs_source_t) -> *mut obs_source_t,
    pub source_muted: unsafe extern "C" fn(source: *const obs_source_t) -> bool,
    pub source_enabled: unsafe extern "C" fn(source: *const obs_source_t) -> bool,
    pub source_update_properties: unsafe extern "C" fn(source: *mut obs_source_t),
    pub properties_create: unsafe extern "C" fn() -> *mut obs_properties_t,
    pub properties_add_text: unsafe extern "C" fn(
        props: *mut obs_properties_t,
        name: *const c_char,
        description: *const c_char,
        r#type: obs_text_type,
    ) -> *mut obs_property_t,
    pub properties_add_bool: unsafe extern "C" fn(
        props: *mut obs_properties_t,
        name: *const c_char,
        description: *const c_char,
    ) -> *mut obs_property_t,
    pub properties_add_list: unsafe extern "C" fn(
        props: *mut obs_properties_t,
        name: *const c_char,
        description: *const c_char,
        r#type: obs_combo_type,
        format: obs_combo_format,
    ) -> *mut obs_property_t,
    pub properties_add_button: unsafe extern "C" fn(
        props: *mut obs_properties_t,
        name: *const c_char,
        text: *const c_char,
        callback: obs_property_clicked_t,
    ) -> *mut obs_property_t,
    pub property_list_add_string: unsafe extern "C" fn(
        p: *mut obs_property_t,
        name: *const c_char,
        val: *const c_char,
    ) -> usize,
    pub property_set_long_description:
        unsafe extern "C" fn(p: *mut obs_property_t, long_description: *const c_char),
    pub property_text_set_info_type:
        unsafe extern "C" fn(p: *mut obs_property_t, r#type: obs_text_info_type),
    pub data_set_default_string:
        unsafe extern "C" fn(data: *mut obs_data_t, name: *const c_char, val: *const c_char),
    pub data_set_default_bool:
        unsafe extern "C" fn(data: *mut obs_data_t, name: *const c_char, val: bool),
    pub data_set_default_double:
        unsafe extern "C" fn(data: *mut obs_data_t, name: *const c_char, val: c_double),
    pub data_get_string:
        unsafe extern "C" fn(data: *mut obs_data_t, name: *const c_char) -> *const c_char,
    pub data_get_bool: unsafe extern "C" fn(data: *mut obs_data_t, name: *const c_char) -> bool,
    pub frontend: Option<ObsFrontendApi>,
}

// SAFETY: Resolved function pointers into libobs are process-global and immutable.
unsafe impl Send for ObsApi {}
unsafe impl Sync for ObsApi {}

static API: OnceLock<ObsApi> = OnceLock::new();

pub fn api() -> Option<&'static ObsApi> {
    API.get()
}

pub fn init_api() -> Result<&'static ObsApi, &'static str> {
    if let Some(existing) = API.get() {
        return Ok(existing);
    }
    let loaded = load_api()?;
    let _ = API.set(loaded);
    API.get().ok_or("failed to store ObsApi")
}

pub fn log(level: c_int, message: &str) {
    let Some(api) = api() else {
        return;
    };
    let formatted = format!("[gstring-captions] {message}");
    if let Ok(cstr) = CString::new(formatted) {
        // SAFETY: `%s` prints the NUL-terminated string without interpreting
        // any `%` characters inside `message`.
        unsafe {
            (api.blog)(level, c"%s".as_ptr(), cstr.as_ptr());
        }
    }
}

/// # Safety
/// `ptr` must either be null or point to a valid NUL-terminated C string.
pub unsafe fn read_c_string(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    // SAFETY: Caller guarantees `ptr` is a valid NUL-terminated C string from OBS.
    unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

#[cfg(windows)]
mod dynload {
    use std::ffi::{CStr, c_char, c_void};

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleHandleA(lpModuleName: *const c_char) -> *mut c_void;
        fn GetProcAddress(hModule: *mut c_void, lpProcName: *const c_char) -> *mut c_void;
    }

    pub struct Lib(*mut c_void);

    pub fn open_obs() -> Option<Lib> {
        // SAFETY: Standard Win32 query for an already-loaded module.
        let handle = unsafe { GetModuleHandleA(c"obs.dll".as_ptr()) };
        if handle.is_null() {
            None
        } else {
            Some(Lib(handle))
        }
    }

    pub fn open_frontend() -> Option<Lib> {
        let handle = unsafe { GetModuleHandleA(c"obs-frontend-api.dll".as_ptr()) };
        if handle.is_null() {
            None
        } else {
            Some(Lib(handle))
        }
    }

    pub unsafe fn sym<T>(lib: &Lib, name: &CStr) -> Option<T> {
        let ptr = unsafe { GetProcAddress(lib.0, name.as_ptr()) };
        if ptr.is_null() {
            None
        } else {
            Some(unsafe { std::mem::transmute_copy(&ptr) })
        }
    }
}

#[cfg(unix)]
mod dynload {
    use std::ffi::{CStr, c_void};

    pub struct Lib;

    pub fn open_obs() -> Option<Lib> {
        Some(Lib)
    }

    pub fn open_frontend() -> Option<Lib> {
        Some(Lib)
    }

    pub unsafe fn sym<T>(_lib: &Lib, name: &CStr) -> Option<T> {
        let ptr = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
        if ptr.is_null() {
            None
        } else {
            Some(unsafe { std::mem::transmute_copy(&ptr) })
        }
    }
}

fn load_api() -> Result<ObsApi, &'static str> {
    let obs = dynload::open_obs().ok_or("obs.dll / libobs is not loaded")?;
    macro_rules! req {
        ($name:literal) => {
            unsafe {
                dynload::sym(
                    &obs,
                    CStr::from_bytes_with_nul_unchecked(concat!($name, "\0").as_bytes()),
                )
            }
            .ok_or(concat!("missing libobs symbol: ", $name))?
        };
    }

    let frontend = dynload::open_frontend().and_then(|fe| unsafe {
        Some(ObsFrontendApi {
            add_event_callback: dynload::sym(&fe, c"obs_frontend_add_event_callback")?,
            remove_event_callback: dynload::sym(&fe, c"obs_frontend_remove_event_callback")?,
            streaming_active: dynload::sym(&fe, c"obs_frontend_streaming_active")?,
            recording_active: dynload::sym(&fe, c"obs_frontend_recording_active")?,
        })
    });

    Ok(ObsApi {
        blog: req!("blog"),
        register_source_s: req!("obs_register_source_s"),
        get_audio: req!("obs_get_audio"),
        audio_output_get_sample_rate: req!("audio_output_get_sample_rate"),
        audio_output_get_channels: req!("audio_output_get_channels"),
        filter_get_parent: req!("obs_filter_get_parent"),
        source_muted: req!("obs_source_muted"),
        source_enabled: req!("obs_source_enabled"),
        source_update_properties: req!("obs_source_update_properties"),
        properties_create: req!("obs_properties_create"),
        properties_add_text: req!("obs_properties_add_text"),
        properties_add_bool: req!("obs_properties_add_bool"),
        properties_add_list: req!("obs_properties_add_list"),
        properties_add_button: req!("obs_properties_add_button"),
        property_list_add_string: req!("obs_property_list_add_string"),
        property_set_long_description: req!("obs_property_set_long_description"),
        property_text_set_info_type: req!("obs_property_text_set_info_type"),
        data_set_default_string: req!("obs_data_set_default_string"),
        data_set_default_bool: req!("obs_data_set_default_bool"),
        data_set_default_double: req!("obs_data_set_default_double"),
        data_get_string: req!("obs_data_get_string"),
        data_get_bool: req!("obs_data_get_bool"),
        frontend,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn obs_audio_data_matches_c_abi() {
        assert_eq!(MAX_AV_PLANES, 8);
        assert!(std::mem::size_of::<obs_audio_data>() >= 8 * std::mem::size_of::<*mut u8>() + 12);
    }

    #[test]
    fn obs_source_info_zeroed_sets_filter_fields_cleanly() {
        let mut info = obs_source_info::zeroed();
        info.r#type = obs_source_type::OBS_SOURCE_TYPE_FILTER;
        info.output_flags = OBS_SOURCE_AUDIO;
        assert_eq!(info.r#type, obs_source_type::OBS_SOURCE_TYPE_FILTER);
        assert_eq!(info.output_flags, 2);
        assert!(info.filter_audio.is_none());
    }
}
