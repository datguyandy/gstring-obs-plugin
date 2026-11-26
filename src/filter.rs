//! OBS Studio audio filter (`gstring_captions_audio_filter`).
//!
//! Attaches to any OBS audio source (Mic, Desktop Audio, Application Audio
//! Capture, etc.), leaves the broadcast audio completely untouched, and
//! streams gated 16 kHz mono PCM to GString's `/overlay/ws` capture endpoint.

use std::ffi::{CString, c_char, c_int, c_void};
use std::panic::catch_unwind;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use bytes::Bytes;
use tokio::sync::{mpsc, watch};

use crate::audio::{PcmResampler, VadGate};
use crate::config::{
    COMPILE_TIME_OVERLAY_URL, CaptureMode, FilterSettings, NoiseGatePreset, parse_connection,
};
use crate::obs_abi::{
    LOG_ERROR, LOG_INFO, LOG_WARNING, MAX_AV_PLANES, OBS_FRONTEND_EVENT_EXIT,
    OBS_FRONTEND_EVENT_RECORDING_STARTED, OBS_FRONTEND_EVENT_RECORDING_STARTING,
    OBS_FRONTEND_EVENT_RECORDING_STOPPED, OBS_FRONTEND_EVENT_RECORDING_STOPPING,
    OBS_FRONTEND_EVENT_STREAMING_STARTED, OBS_FRONTEND_EVENT_STREAMING_STARTING,
    OBS_FRONTEND_EVENT_STREAMING_STOPPED, OBS_FRONTEND_EVENT_STREAMING_STOPPING, OBS_SOURCE_AUDIO,
    ObsApi, api, log, obs_audio_data, obs_combo_format, obs_combo_type, obs_data_t,
    obs_properties_t, obs_property_t, obs_source_info, obs_source_t, obs_source_type,
    obs_text_info_type, obs_text_type, read_c_string,
};
use crate::worker::{
    DesiredState, RuntimeSignals, StatusSink, StatusTone, WorkerStatus, run_worker,
};

const PROP_STATUS: &std::ffi::CStr = c"status_info";
const PROP_CAPTURE_KEY: &std::ffi::CStr = c"capture_key";
const PROP_SERVER_URL: &std::ffi::CStr = c"server_url";
const PROP_CAPTURE_MODE: &std::ffi::CStr = c"capture_mode";
const PROP_RESPECT_MUTE: &std::ffi::CStr = c"respect_obs_mute";
const PROP_NOISE_GATE: &std::ffi::CStr = c"noise_gate";
const PROP_RECONNECT: &std::ffi::CStr = c"reconnect_now";

/// Capacity of the audio-thread -> worker channel (~3.2 seconds of 100 ms frames).
const AUDIO_QUEUE_CAPACITY: usize = 32;

/// Global OBS streaming/recording flags updated from `obs-frontend-api` events.
static FRONTEND_STREAMING: AtomicBool = AtomicBool::new(false);
static FRONTEND_RECORDING: AtomicBool = AtomicBool::new(false);
static FRONTEND_HOOKED: AtomicBool = AtomicBool::new(false);

/// Shared state touched by OBS UI callbacks and frontend events.
struct FilterControl {
    source: usize,
    settings: Mutex<FilterSettings>,
    state_tx: watch::Sender<DesiredState>,
    runtime: Arc<RuntimeSignals>,
    status: Arc<StatusSink>,
    /// Cached bit-exact noise floor read on the audio thread without locking.
    noise_floor_bits: AtomicU32,
    /// Cached boolean read on the audio thread without locking.
    respect_obs_mute: AtomicBool,
    /// Cached capture mode discriminant (`0` Always, `1` Streaming, `2` StreamOrRec).
    capture_mode: AtomicU32,
    /// Monotonic counter bumped by "Reconnect now".
    reconnect_nonce: AtomicU64,
}

impl FilterControl {
    fn source_ptr(&self) -> *mut obs_source_t {
        self.source as *mut obs_source_t
    }

    fn mode(&self) -> CaptureMode {
        match self.capture_mode.load(Ordering::Relaxed) {
            0 => CaptureMode::Always,
            2 => CaptureMode::StreamingOrRecording,
            _ => CaptureMode::StreamingOnly,
        }
    }

    fn is_source_enabled(&self) -> bool {
        let Some(obs) = api() else {
            return true;
        };
        let src = self.source_ptr();
        if src.is_null() {
            return true;
        }
        unsafe { (obs.source_enabled)(src) }
    }

    fn is_parent_muted(&self) -> bool {
        if !self.respect_obs_mute.load(Ordering::Relaxed) {
            return false;
        }
        let Some(obs) = api() else {
            return false;
        };
        let src = self.source_ptr();
        if src.is_null() {
            return false;
        }
        let parent = unsafe { (obs.filter_get_parent)(src) };
        if parent.is_null() {
            return false;
        }
        unsafe { (obs.source_muted)(parent) }
    }

    fn compute_desired(&self, settings: &FilterSettings) -> DesiredState {
        let streaming = FRONTEND_STREAMING.load(Ordering::Relaxed);
        let recording = FRONTEND_RECORDING.load(Ordering::Relaxed);
        let enabled = self.is_source_enabled();
        let mode_ok = settings.capture_mode.is_active(streaming, recording);
        let inactive_reason = match settings.capture_mode {
            CaptureMode::StreamingOnly => "Idle — waiting for OBS stream to start.".to_owned(),
            CaptureMode::StreamingOrRecording => {
                "Idle — waiting for OBS stream or recording to start.".to_owned()
            }
            CaptureMode::Always => "Idle.".to_owned(),
        };
        let connection = parse_connection(&settings.capture_key, &settings.server_url)
            .map_err(|e| e.message().to_owned());
        let muted = self.is_parent_muted();
        self.runtime.update_from_audio_thread(enabled, muted);
        DesiredState {
            connection,
            active: mode_ok,
            inactive_reason,
            muted,
            reconnect_nonce: self.reconnect_nonce.load(Ordering::Relaxed),
        }
    }

    fn sync_from_settings(&self, next: FilterSettings) {
        self.noise_floor_bits
            .store(next.noise_gate.rms_threshold().to_bits(), Ordering::Relaxed);
        self.respect_obs_mute
            .store(next.respect_obs_mute, Ordering::Relaxed);
        self.capture_mode.store(
            match next.capture_mode {
                CaptureMode::Always => 0,
                CaptureMode::StreamingOnly => 1,
                CaptureMode::StreamingOrRecording => 2,
            },
            Ordering::Relaxed,
        );
        let desired = self.compute_desired(&next);
        if let Ok(mut guard) = self.settings.lock() {
            *guard = next;
        }
        let _ = self.state_tx.send(desired);
    }

    fn refresh_runtime_state(&self) {
        let settings = match self.settings.lock() {
            Ok(g) => g.clone(),
            Err(_) => return,
        };
        let desired = self.compute_desired(&settings);
        let _ = self.state_tx.send(desired);
    }

    fn request_reconnect(&self) {
        self.reconnect_nonce.fetch_add(1, Ordering::Relaxed);
        self.refresh_runtime_state();
    }
}

/// Registry of active filter instances so frontend streaming/recording events
/// wake every filter immediately.
static ACTIVE_FILTERS: Mutex<Vec<Arc<FilterControl>>> = Mutex::new(Vec::new());

unsafe extern "C" fn on_frontend_event(event: c_int, _private_data: *mut c_void) {
    let _ = catch_unwind(|| {
        let relevant = match event {
            OBS_FRONTEND_EVENT_STREAMING_STARTING | OBS_FRONTEND_EVENT_STREAMING_STARTED => {
                FRONTEND_STREAMING.store(true, Ordering::Relaxed);
                true
            }
            OBS_FRONTEND_EVENT_STREAMING_STOPPING | OBS_FRONTEND_EVENT_STREAMING_STOPPED => {
                FRONTEND_STREAMING.store(false, Ordering::Relaxed);
                true
            }
            OBS_FRONTEND_EVENT_RECORDING_STARTING | OBS_FRONTEND_EVENT_RECORDING_STARTED => {
                FRONTEND_RECORDING.store(true, Ordering::Relaxed);
                true
            }
            OBS_FRONTEND_EVENT_RECORDING_STOPPING | OBS_FRONTEND_EVENT_RECORDING_STOPPED => {
                FRONTEND_RECORDING.store(false, Ordering::Relaxed);
                true
            }
            OBS_FRONTEND_EVENT_EXIT => {
                FRONTEND_STREAMING.store(false, Ordering::Relaxed);
                FRONTEND_RECORDING.store(false, Ordering::Relaxed);
                true
            }
            _ => false,
        };
        if relevant && let Ok(list) = ACTIVE_FILTERS.lock() {
            for ctrl in list.iter() {
                ctrl.refresh_runtime_state();
            }
        }
    });
}

pub fn register_frontend_hooks(obs: &ObsApi) {
    let Some(fe) = &obs.frontend else {
        // Headless libobs without frontend: treat as always streaming if user
        // picks streaming mode? No, leave false; Always mode works everywhere.
        return;
    };
    if FRONTEND_HOOKED.swap(true, Ordering::SeqCst) {
        return;
    }
    unsafe {
        FRONTEND_STREAMING.store((fe.streaming_active)(), Ordering::Relaxed);
        FRONTEND_RECORDING.store((fe.recording_active)(), Ordering::Relaxed);
        (fe.add_event_callback)(Some(on_frontend_event), std::ptr::null_mut());
    }
}

pub fn unregister_frontend_hooks() {
    if !FRONTEND_HOOKED.swap(false, Ordering::SeqCst) {
        return;
    }
    if let Some(fe) = api().and_then(|a| a.frontend.as_ref()) {
        unsafe {
            (fe.remove_event_callback)(Some(on_frontend_event), std::ptr::null_mut());
        }
    }
}

/// Audio-thread DSP state isolated behind a dedicated `Mutex` (only accessed
/// via non-blocking `try_lock()` inside `filter_audio`) so every OBS callback
/// only ever creates a shared `&FilterInstance` — preventing cross-thread
/// `&mut` / `&` aliasing UB with UI callbacks.
struct AudioState {
    resampler: PcmResampler,
    vad: VadGate,
}

/// Per-filter instance owned by OBS between `create` and `destroy`.
struct FilterInstance {
    control: Arc<FilterControl>,
    audio: Mutex<AudioState>,
    /// Non-blocking bounded sender to the background WebSocket worker.
    audio_tx: mpsc::Sender<Bytes>,
    /// Background worker OS thread handle (joined on `destroy`).
    worker_thread: Option<JoinHandle<()>>,
}

fn read_settings(obs: &ObsApi, data: *mut obs_data_t) -> FilterSettings {
    if data.is_null() {
        return FilterSettings::default();
    }
    unsafe {
        let capture_key = read_c_string((obs.data_get_string)(data, PROP_CAPTURE_KEY.as_ptr()));
        let server_url = read_c_string((obs.data_get_string)(data, PROP_SERVER_URL.as_ptr()));
        let mode_str = read_c_string((obs.data_get_string)(data, PROP_CAPTURE_MODE.as_ptr()));
        let respect_obs_mute = (obs.data_get_bool)(data, PROP_RESPECT_MUTE.as_ptr());
        let gate_str = read_c_string((obs.data_get_string)(data, PROP_NOISE_GATE.as_ptr()));
        FilterSettings {
            capture_key,
            server_url,
            capture_mode: CaptureMode::from_setting(&mode_str),
            respect_obs_mute,
            noise_gate: NoiseGatePreset::from_setting(&gate_str),
        }
    }
}

unsafe extern "C" fn filter_get_name(_type_data: *mut c_void) -> *const c_char {
    c"GString Captions".as_ptr()
}

unsafe extern "C" fn filter_get_defaults(settings: *mut obs_data_t) {
    let _ = catch_unwind(|| {
        let Some(obs) = api() else {
            return;
        };
        if settings.is_null() {
            return;
        }
        let default_url = CString::new(COMPILE_TIME_OVERLAY_URL.unwrap_or("")).unwrap_or_default();
        unsafe {
            (obs.data_set_default_string)(settings, PROP_CAPTURE_KEY.as_ptr(), c"".as_ptr());
            (obs.data_set_default_string)(settings, PROP_SERVER_URL.as_ptr(), default_url.as_ptr());
            (obs.data_set_default_string)(
                settings,
                PROP_CAPTURE_MODE.as_ptr(),
                c"streaming".as_ptr(),
            );
            (obs.data_set_default_bool)(settings, PROP_RESPECT_MUTE.as_ptr(), true);
            (obs.data_set_default_string)(settings, PROP_NOISE_GATE.as_ptr(), c"normal".as_ptr());
        }
    });
}

unsafe extern "C" fn on_reconnect_clicked(
    _props: *mut obs_properties_t,
    _property: *mut obs_property_t,
    data: *mut c_void,
) -> bool {
    catch_unwind(|| {
        if data.is_null() {
            return false;
        }
        let instance = unsafe { &*(data as *const FilterInstance) };
        instance.control.request_reconnect();
        true
    })
    .unwrap_or(false)
}

unsafe extern "C" fn filter_get_properties(data: *mut c_void) -> *mut obs_properties_t {
    catch_unwind(|| {
        let Some(obs) = api() else {
            return std::ptr::null_mut();
        };
        unsafe {
            let props = (obs.properties_create)();

            let status_snapshot = if data.is_null() {
                WorkerStatus::normal("Not configured — paste your OBS plugin key below.")
            } else {
                (&*(data as *const FilterInstance)).control.status.get()
            };
            let status_cstr = CString::new(status_snapshot.text)
                .unwrap_or_else(|_| CString::new("Status unavailable").unwrap_or_default());
            let status_prop = (obs.properties_add_text)(
                props,
                PROP_STATUS.as_ptr(),
                status_cstr.as_ptr(),
                obs_text_type::OBS_TEXT_INFO,
            );
            (obs.property_text_set_info_type)(
                status_prop,
                match status_snapshot.tone {
                    StatusTone::Normal => obs_text_info_type::OBS_TEXT_INFO_NORMAL,
                    StatusTone::Warning => obs_text_info_type::OBS_TEXT_INFO_WARNING,
                    StatusTone::Error => obs_text_info_type::OBS_TEXT_INFO_ERROR,
                },
            );

            let key_prop = (obs.properties_add_text)(
                props,
                PROP_CAPTURE_KEY.as_ptr(),
                c"OBS plugin key".as_ptr(),
                obs_text_type::OBS_TEXT_PASSWORD,
            );
            (obs.property_set_long_description)(
                key_prop,
                c"Copy this key from Captions -> OBS setup in the GString dashboard. Resetting links revokes it."
                    .as_ptr(),
            );

            let mode_prop = (obs.properties_add_list)(
                props,
                PROP_CAPTURE_MODE.as_ptr(),
                c"Capture when".as_ptr(),
                obs_combo_type::OBS_COMBO_TYPE_LIST,
                obs_combo_format::OBS_COMBO_FORMAT_STRING,
            );
            (obs.property_list_add_string)(
                mode_prop,
                c"Only while streaming (recommended)".as_ptr(),
                c"streaming".as_ptr(),
            );
            (obs.property_list_add_string)(
                mode_prop,
                c"While streaming or recording".as_ptr(),
                c"streaming_or_recording".as_ptr(),
            );
            (obs.property_list_add_string)(
                mode_prop,
                c"Always (while filter is enabled)".as_ptr(),
                c"always".as_ptr(),
            );

            (obs.properties_add_bool)(
                props,
                PROP_RESPECT_MUTE.as_ptr(),
                c"Pause captions when this OBS audio source is muted".as_ptr(),
            );

            let gate_prop = (obs.properties_add_list)(
                props,
                PROP_NOISE_GATE.as_ptr(),
                c"Noise gate".as_ptr(),
                obs_combo_type::OBS_COMBO_TYPE_LIST,
                obs_combo_format::OBS_COMBO_FORMAT_STRING,
            );
            (obs.property_list_add_string)(
                gate_prop,
                c"Low (quiet microphone)".as_ptr(),
                c"low".as_ptr(),
            );
            (obs.property_list_add_string)(
                gate_prop,
                c"Normal (default)".as_ptr(),
                c"normal".as_ptr(),
            );
            (obs.property_list_add_string)(
                gate_prop,
                c"High (noisy room / desktop audio)".as_ptr(),
                c"high".as_ptr(),
            );

            let server_prop = (obs.properties_add_text)(
                props,
                PROP_SERVER_URL.as_ptr(),
                c"Server URL override (optional)".as_ptr(),
                obs_text_type::OBS_TEXT_DEFAULT,
            );
            (obs.property_set_long_description)(
                server_prop,
                c"Leave blank when using a full key copied from the GString dashboard. Only needed for custom or local servers."
                    .as_ptr(),
            );

            (obs.properties_add_button)(
                props,
                PROP_RECONNECT.as_ptr(),
                c"Reconnect now".as_ptr(),
                Some(on_reconnect_clicked),
            );

            props
        }
    })
    .unwrap_or(std::ptr::null_mut())
}

unsafe extern "C" fn filter_create(
    settings: *mut obs_data_t,
    source: *mut obs_source_t,
) -> *mut c_void {
    catch_unwind(|| {
        let Some(obs) = api() else {
            return std::ptr::null_mut();
        };

        let initial_settings = read_settings(obs, settings);
        let source_addr = source as usize;

        let status = Arc::new(StatusSink::new(
            WorkerStatus::normal("Starting…"),
            Arc::new(move |next| {
                let level = match next.tone {
                    StatusTone::Normal => LOG_INFO,
                    StatusTone::Warning => LOG_WARNING,
                    StatusTone::Error => LOG_ERROR,
                };
                log(level, &next.text);
                if let Some(obs) = api() {
                    let src = source_addr as *mut obs_source_t;
                    if !src.is_null() {
                        unsafe { (obs.source_update_properties)(src) };
                    }
                }
            }),
        ));

        let placeholder = DesiredState {
            connection: Err("Starting…".to_owned()),
            active: false,
            inactive_reason: "Starting…".to_owned(),
            muted: false,
            reconnect_nonce: 0,
        };
        let (state_tx, state_rx) = watch::channel(placeholder);
        let runtime = Arc::new(RuntimeSignals::default());
        let (audio_tx, audio_rx) = mpsc::channel(AUDIO_QUEUE_CAPACITY);

        let control = Arc::new(FilterControl {
            source: source_addr,
            settings: Mutex::new(initial_settings.clone()),
            state_tx,
            runtime: Arc::clone(&runtime),
            status: Arc::clone(&status),
            noise_floor_bits: AtomicU32::new(initial_settings.noise_gate.rms_threshold().to_bits()),
            respect_obs_mute: AtomicBool::new(initial_settings.respect_obs_mute),
            capture_mode: AtomicU32::new(match initial_settings.capture_mode {
                CaptureMode::Always => 0,
                CaptureMode::StreamingOnly => 1,
                CaptureMode::StreamingOrRecording => 2,
            }),
            reconnect_nonce: AtomicU64::new(0),
        });

        control.sync_from_settings(initial_settings);

        if let Ok(mut list) = ACTIVE_FILTERS.lock() {
            list.push(Arc::clone(&control));
        }

        let worker_status = Arc::clone(&status);
        let worker_runtime = Arc::clone(&runtime);
        let worker_thread = thread::Builder::new()
            .name("gstring-captions-ws".to_owned())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build();
                match rt {
                    Ok(rt) => rt.block_on(run_worker(
                        state_rx,
                        worker_runtime,
                        audio_rx,
                        worker_status,
                    )),
                    Err(err) => {
                        worker_status.set(WorkerStatus::error(format!(
                            "Could not start captions network runtime: {err}"
                        )));
                    }
                }
            })
            .ok();

        let instance = Box::new(FilterInstance {
            control,
            audio: Mutex::new(AudioState {
                resampler: PcmResampler::default(),
                vad: VadGate::default(),
            }),
            audio_tx,
            worker_thread,
        });
        Box::into_raw(instance) as *mut c_void
    })
    .unwrap_or(std::ptr::null_mut())
}

unsafe extern "C" fn filter_update(data: *mut c_void, settings: *mut obs_data_t) {
    let _ = catch_unwind(|| {
        if data.is_null() {
            return;
        }
        let Some(obs) = api() else {
            return;
        };
        let instance = unsafe { &*(data as *const FilterInstance) };
        let next = read_settings(obs, settings);
        instance.control.sync_from_settings(next);
    });
}

unsafe extern "C" fn filter_destroy(data: *mut c_void) {
    let _ = catch_unwind(|| {
        if data.is_null() {
            return;
        }
        let mut instance = unsafe { Box::from_raw(data as *mut FilterInstance) };
        if let Ok(mut list) = ACTIVE_FILTERS.lock() {
            list.retain(|item| !Arc::ptr_eq(item, &instance.control));
        }
        let thread_handle = instance.worker_thread.take();
        // Dropping `instance` drops `state_tx` and `audio_tx`, signalling the
        // worker to send `{"type":"stop"}` and exit cleanly.
        drop(instance);
        if let Some(handle) = thread_handle {
            let _ = handle.join();
        }
    });
}

unsafe extern "C" fn filter_audio(
    data: *mut c_void,
    audio: *mut obs_audio_data,
) -> *mut obs_audio_data {
    let _ = catch_unwind(|| {
        if data.is_null() || audio.is_null() {
            return;
        }
        let Some(obs) = api() else {
            return;
        };
        // Shared reference only: UI callbacks concurrently hold `&FilterInstance`.
        let instance = unsafe { &*(data as *const FilterInstance) };

        // Lock-free mute & filter-enabled sync (never acquires `settings` mutex
        // or re-parses URLs on the real-time OBS audio thread).
        let streaming = FRONTEND_STREAMING.load(Ordering::Relaxed);
        let recording = FRONTEND_RECORDING.load(Ordering::Relaxed);
        let enabled = instance.control.is_source_enabled();
        let active = enabled && instance.control.mode().is_active(streaming, recording);
        let muted = instance.control.is_parent_muted();
        instance
            .control
            .runtime
            .update_from_audio_thread(enabled, muted);

        let Ok(mut dsp) = instance.audio.try_lock() else {
            return;
        };

        if !active || muted {
            dsp.resampler.reset();
            dsp.vad.reset();
            return;
        }

        let obs_audio = unsafe { (obs.get_audio)() };
        if obs_audio.is_null() {
            return;
        }
        let sample_rate = unsafe { (obs.audio_output_get_sample_rate)(obs_audio) };
        let channels = unsafe { (obs.audio_output_get_channels)(obs_audio) }.min(MAX_AV_PLANES);
        let audio_ref = unsafe { &*audio };
        let frames = audio_ref.frames as usize;
        if sample_rate == 0 || channels == 0 || frames == 0 {
            return;
        }

        let mut planes: [&[f32]; MAX_AV_PLANES] = [&[]; MAX_AV_PLANES];
        let mut plane_count = 0usize;
        for &raw in audio_ref.data.iter().take(channels) {
            let ptr = raw as *const f32;
            if !ptr.is_null() {
                planes[plane_count] = unsafe { std::slice::from_raw_parts(ptr, frames) };
                plane_count += 1;
            }
        }
        if plane_count == 0 {
            return;
        }

        let floor = f32::from_bits(instance.control.noise_floor_bits.load(Ordering::Relaxed));
        dsp.vad.set_noise_floor(floor);

        let AudioState { resampler, vad } = &mut *dsp;
        let tx = &instance.audio_tx;
        resampler.push_planar(&planes[..plane_count], sample_rate, |chunk| {
            vad.push(chunk, |pcm| {
                // Non-blocking: never stall the real-time OBS audio thread.
                let _ = tx.try_send(pcm);
            });
        });
    });

    // Pass-through: never alter the streamer's broadcast audio.
    audio
}

pub fn register_filter(obs: &ObsApi) {
    let mut info = obs_source_info::zeroed();
    info.id = c"gstring_captions_audio_filter".as_ptr();
    info.r#type = obs_source_type::OBS_SOURCE_TYPE_FILTER;
    info.output_flags = OBS_SOURCE_AUDIO;
    info.get_name = Some(filter_get_name);
    info.create = Some(filter_create);
    info.destroy = Some(filter_destroy);
    info.get_defaults = Some(filter_get_defaults);
    info.get_properties = Some(filter_get_properties);
    info.update = Some(filter_update);
    info.filter_audio = Some(filter_audio);

    unsafe {
        (obs.register_source_s)(&info, std::mem::size_of::<obs_source_info>());
    }
}
