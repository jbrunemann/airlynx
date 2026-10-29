//! Android glue: JNI calls into the framework for things NativeActivity lacks.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use winit::platform::android::activity::AndroidApp;
use airlynx_core::DeviceKind;
use jni::objects::{GlobalRef, JObject, JObjectArray, JString, JValue};
use jni::{JNIEnv, JavaVM};

static APP: OnceLock<AndroidApp> = OnceLock::new();

/// Which locks `set_keep_awake` takes. All of them by default; the Diagnostics card turns them off one by
/// one to measure what each costs in battery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Keep {
    pub wake_lock: bool,
    pub wifi_lock: bool,
    pub multicast_lock: bool,
}

impl Default for Keep {
    fn default() -> Self {
        Self { wake_lock: true, wifi_lock: true, multicast_lock: true }
    }
}

static KEEP: Mutex<Keep> = Mutex::new(Keep { wake_lock: true, wifi_lock: true, multicast_lock: true });

#[derive(Default)]
struct Held {
    /// The foreground service is running.
    active: bool,
    /// Locks taken while receiving, by name.
    locks: Vec<(&'static str, GlobalRef)>,
}

static HELD: Mutex<Held> = Mutex::new(Held { active: false, locks: Vec::new() });

pub fn init(app: AndroidApp) {
    let _ = APP.set(app);
}

/// Runs `f` with a JNI env and the activity, logging and clearing any Java exception.
fn with_env<R>(f: impl FnOnce(&mut JNIEnv<'_>, &JObject<'_>) -> jni::errors::Result<R>) -> Option<R> {
    let app = APP.get()?;
    let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) }.ok()?;
    let mut env = vm.attach_current_thread().ok()?;
    let activity = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
    let result = f(&mut env, &activity);
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_describe();
        let _ = env.exception_clear();
    }
    result.map_err(|e| log::warn!("JNI call failed: {e}")).ok()
}

fn java_string(env: &mut JNIEnv<'_>, obj: JObject<'_>) -> jni::errors::Result<String> {
    Ok(env.get_string(&JString::from(obj))?.into())
}

pub fn device_kind() -> DeviceKind {
    DeviceKind::Mobile
}

pub fn device_name() -> String {
    with_env(|env, _| {
        let model = env.get_static_field("android/os/Build", "MODEL", "Ljava/lang/String;")?.l()?;
        java_string(env, model)
    })
    .unwrap_or_else(|| "Android".into())
}

pub fn storage_root() -> PathBuf {
    PathBuf::from("/storage/emulated/0")
}

/// Download/AirLynx with all-files access, otherwise the app's own external folder
/// (Android/data/<package>/files), which needs no permission.
pub fn default_receive_dir() -> PathBuf {
    if has_all_files_access() {
        return storage_root().join("Download").join("AirLynx");
    }
    APP.get()
        .and_then(|app| app.external_data_path().or_else(|| app.internal_data_path()))
        .unwrap_or_else(|| PathBuf::from("/data/local/tmp"))
        .join("received")
}

/// Where the device id and favorites live: the app's private storage, which needs no permission.
pub fn config_dir() -> PathBuf {
    APP.get()
        .and_then(|app| app.internal_data_path())
        .unwrap_or_else(|| PathBuf::from("/data/local/tmp"))
        .join("config")
}

/// What other apps shared to AirLynx since the last call. `MainActivity` (app/java) copies the content
/// URIs it receives into the cache and keeps the paths until they are taken.
#[derive(Default)]
pub struct SharedFiles {
    /// Copied files, ready to send.
    pub ready: Vec<PathBuf>,
    /// Items still being copied.
    pub preparing: usize,
    /// Why a copy failed, if one did.
    pub error: Option<String>,
}

/// Asks `MainActivity` for shared files. The activity is the object NativeActivity runs in, so its
/// methods are called directly (no class lookup, which a native thread could not do for app classes).
pub fn take_shared() -> SharedFiles {
    with_env(|env, activity| {
        let paths = JObjectArray::from(env.call_method(activity, "takeSharedFiles", "()[Ljava/lang/String;", &[])?.l()?);
        let mut ready = Vec::new();
        for i in 0..env.get_array_length(&paths)? {
            let path = env.get_object_array_element(&paths, i)?;
            ready.push(PathBuf::from(java_string(env, path)?));
        }
        let preparing = env.call_method(activity, "sharedPreparing", "()I", &[])?.i()?;
        let error = env.call_method(activity, "takeShareError", "()Ljava/lang/String;", &[])?.l()?;
        let error = if error.is_null() { None } else { Some(java_string(env, error)?) };
        Ok(SharedFiles { ready, preparing: preparing.max(0) as usize, error })
    })
    .unwrap_or_default()
}

/// Deletes `path` if it is a copy of a shared file; any other path is left alone.
pub fn discard_copy(path: &Path) {
    with_env(|env, activity| {
        let path = env.new_string(path.to_string_lossy())?;
        env.call_method(activity, "discardShared", "(Ljava/lang/String;)V", &[JValue::Object(&path)])?;
        Ok(())
    });
}

pub fn has_all_files_access() -> bool {
    with_env(|env, _| {
        env.call_static_method("android/os/Environment", "isExternalStorageManager", "()Z", &[])?
            .z()
    })
    .unwrap_or(false)
}

/// Opens the system "All files access" settings page for this app.
pub fn request_all_files_access() {
    with_env(|env, activity| {
        let package = env.call_method(activity, "getPackageName", "()Ljava/lang/String;", &[])?.l()?;
        let package = java_string(env, package)?;
        let uri_text = env.new_string(format!("package:{package}"))?;
        let uri = env
            .call_static_method(
                "android/net/Uri",
                "parse",
                "(Ljava/lang/String;)Landroid/net/Uri;",
                &[JValue::Object(&uri_text)],
            )?
            .l()?;
        let action = env.new_string("android.settings.MANAGE_APP_ALL_FILES_ACCESS_PERMISSION")?;
        let intent = env.new_object(
            "android/content/Intent",
            "(Ljava/lang/String;Landroid/net/Uri;)V",
            &[JValue::Object(&action), JValue::Object(&uri)],
        )?;
        env.call_method(activity, "startActivity", "(Landroid/content/Intent;)V", &[JValue::Object(&intent)])?;
        Ok(())
    });
}

fn system_service<'a>(env: &mut JNIEnv<'a>, context: &JObject<'_>, name: &str) -> jni::errors::Result<JObject<'a>> {
    let name = env.new_string(name)?;
    env.call_method(
        context,
        "getSystemService",
        "(Ljava/lang/String;)Ljava/lang/Object;",
        &[JValue::Object(&name)],
    )?
    .l()
}

/// Intent for `ReceiveService` (app/java). Addressed by name because a native thread's class loader
/// cannot see app classes, so `Class.forName` would fail here.
fn receive_service_intent<'a>(env: &mut JNIEnv<'a>, activity: &JObject<'_>) -> jni::errors::Result<JObject<'a>> {
    let package = env.call_method(activity, "getPackageName", "()Ljava/lang/String;", &[])?.l()?;
    let class = env.new_string("dev.airlynx.app.ReceiveService")?;
    let intent = env.new_object("android/content/Intent", "()V", &[])?;
    env.call_method(
        &intent,
        "setClassName",
        "(Ljava/lang/String;Ljava/lang/String;)Landroid/content/Intent;",
        &[JValue::Object(&package), JValue::Object(&class)],
    )?;
    Ok(intent)
}

/// Android 13+ hides notifications, including the one a foreground service must show, until the user allows them.
fn request_notification_permission(env: &mut JNIEnv<'_>, activity: &JObject<'_>) -> jni::errors::Result<()> {
    let permission = env.new_string("android.permission.POST_NOTIFICATIONS")?;
    let state = env
        .call_method(activity, "checkSelfPermission", "(Ljava/lang/String;)I", &[JValue::Object(&permission)])?
        .i()?;
    if state == 0 {
        return Ok(());
    }
    let list = env.new_object_array(1, "java/lang/String", &permission)?;
    env.call_method(
        activity,
        "requestPermissions",
        "([Ljava/lang/String;I)V",
        &[JValue::Object(&list), JValue::Int(1)],
    )?;
    Ok(())
}

/// Starts or stops the foreground service that keeps the process running in the background.
/// Wake and Wi-Fi locks alone do not: once the app is backgrounded, Android can freeze it or cut its
/// network access (Doze ignores partial wake locks), so a locked phone would stop answering senders.
fn set_receive_service(on: bool) {
    with_env(|env, activity| {
        let intent = receive_service_intent(env, activity)?;
        if on {
            if let Err(e) = request_notification_permission(env, activity) {
                log::warn!("could not request notification permission: {e}");
                let _ = env.exception_clear();
            }
            env.call_method(
                activity,
                "startForegroundService",
                "(Landroid/content/Intent;)Landroid/content/ComponentName;",
                &[JValue::Object(&intent)],
            )?;
        } else {
            env.call_method(activity, "stopService", "(Landroid/content/Intent;)Z", &[JValue::Object(&intent)])?;
        }
        Ok(())
    });
}

/// Chooses which locks the next `set_keep_awake(true)` takes.
pub fn set_keep(keep: Keep) {
    *KEEP.lock().unwrap() = keep;
}

/// Sets reference counting off, so one `release` always lets go, then acquires.
fn acquire(env: &mut JNIEnv<'_>, lock: &JObject<'_>) -> jni::errors::Result<GlobalRef> {
    env.call_method(lock, "setReferenceCounted", "(Z)V", &[JValue::Bool(0)])?;
    env.call_method(lock, "acquire", "()V", &[])?;
    env.new_global_ref(lock)
}

/// Keeps the app able to receive with the screen locked: a foreground service (so the process is not
/// frozen or cut off from the network), plus the locks chosen with `set_keep`. A CPU wake lock keeps
/// incoming connections from stalling, a Wi-Fi lock keeps the radio awake, and a multicast lock makes
/// the radio deliver the broadcast packets that discovery uses (many devices drop them otherwise).
pub fn set_keep_awake(on: bool) {
    let mut held = HELD.lock().unwrap();
    if !on {
        set_receive_service(false);
        held.active = false;
        for (_, lock) in held.locks.drain(..) {
            with_env(|env, _| {
                if env.call_method(&lock, "isHeld", "()Z", &[])?.z()? {
                    env.call_method(&lock, "release", "()V", &[])?;
                }
                Ok(())
            });
        }
        return;
    }
    if held.active {
        return;
    }
    held.active = true;
    let keep = *KEEP.lock().unwrap();
    set_receive_service(true);
    with_env(|env, activity| {
        let context = env
            .call_method(activity, "getApplicationContext", "()Landroid/content/Context;", &[])?
            .l()?;
        if keep.wake_lock {
            let power = system_service(env, &context, "power")?;
            let tag = env.new_string("airlynx:receive")?;
            // PARTIAL_WAKE_LOCK = 1
            let wake = env
                .call_method(
                    &power,
                    "newWakeLock",
                    "(ILjava/lang/String;)Landroid/os/PowerManager$WakeLock;",
                    &[JValue::Int(1), JValue::Object(&tag)],
                )?
                .l()?;
            held.locks.push(("wake", acquire(env, &wake)?));
        }
        if keep.wifi_lock || keep.multicast_lock {
            let wifi = system_service(env, &context, "wifi")?;
            let tag = env.new_string("airlynx")?;
            if keep.wifi_lock {
                // WIFI_MODE_FULL_HIGH_PERF = 3 (deprecated since API 34 but still honored)
                let lock = env
                    .call_method(
                        &wifi,
                        "createWifiLock",
                        "(ILjava/lang/String;)Landroid/net/wifi/WifiManager$WifiLock;",
                        &[JValue::Int(3), JValue::Object(&tag)],
                    )?
                    .l()?;
                held.locks.push(("wifi", acquire(env, &lock)?));
            }
            if keep.multicast_lock {
                let lock = env
                    .call_method(
                        &wifi,
                        "createMulticastLock",
                        "(Ljava/lang/String;)Landroid/net/wifi/WifiManager$MulticastLock;",
                        &[JValue::Object(&tag)],
                    )?
                    .l()?;
                held.locks.push(("multicast", acquire(env, &lock)?));
            }
        }
        Ok(())
    });
}

/// Names of the locks currently held.
pub fn held_locks() -> Vec<&'static str> {
    HELD.lock().unwrap().locks.iter().map(|(name, _)| *name).collect()
}

/// One entry of Android's record of how an earlier process of this app ended (`ApplicationExitInfo`).
#[derive(Clone, Debug)]
pub struct ExitRecord {
    pub unix_ms: i64,
    pub reason: i32,
    /// The signal number when the reason is `signaled` (9 is SIGKILL), otherwise the exit code.
    pub status: i32,
    /// `ActivityManager.RunningAppProcessInfo` importance when the process died: 125 foreground
    /// service, 400 cached.
    pub importance: i32,
    pub description: String,
}

/// The most recent exits of this app's processes, newest first. Android keeps them across restarts,
/// which makes this the way to learn why the receiver stopped while nobody was looking.
pub fn exit_reasons(max: i32) -> Vec<ExitRecord> {
    with_env(|env, activity| {
        let context = env
            .call_method(activity, "getApplicationContext", "()Landroid/content/Context;", &[])?
            .l()?;
        let package = env.call_method(&context, "getPackageName", "()Ljava/lang/String;", &[])?.l()?;
        let manager = system_service(env, &context, "activity")?;
        let list = env
            .call_method(
                &manager,
                "getHistoricalProcessExitReasons",
                "(Ljava/lang/String;II)Ljava/util/List;",
                &[JValue::Object(&package), JValue::Int(0), JValue::Int(max)],
            )?
            .l()?;
        let size = env.call_method(&list, "size", "()I", &[])?.i()?;
        let mut records = Vec::new();
        for i in 0..size {
            let info = env.call_method(&list, "get", "(I)Ljava/lang/Object;", &[JValue::Int(i)])?.l()?;
            let description = env.call_method(&info, "getDescription", "()Ljava/lang/String;", &[])?.l()?;
            records.push(ExitRecord {
                unix_ms: env.call_method(&info, "getTimestamp", "()J", &[])?.j()?,
                reason: env.call_method(&info, "getReason", "()I", &[])?.i()?,
                status: env.call_method(&info, "getStatus", "()I", &[])?.i()?,
                importance: env.call_method(&info, "getImportance", "()I", &[])?.i()?,
                description: if description.is_null() { String::new() } else { java_string(env, description)? },
            });
        }
        Ok(records)
    })
    .unwrap_or_default()
}

/// Whether the screen is on. One cheap call, for the logging thread to poll.
pub fn screen_on() -> Option<bool> {
    with_env(|env, activity| {
        let context = env
            .call_method(activity, "getApplicationContext", "()Landroid/content/Context;", &[])?
            .l()?;
        let power = system_service(env, &context, "power")?;
        env.call_method(&power, "isInteractive", "()Z", &[])?.z()
    })
}

/// Battery and power state, for the Diagnostics card.
#[derive(Clone, Copy, Debug)]
pub struct PowerInfo {
    pub percent: i32,
    /// Plugged into a charger or a dock, so the drain figures mean nothing.
    pub charging: bool,
    /// Charge left in microampere-hours. Much finer than `percent`, but not every device reports it.
    pub charge_uah: Option<i64>,
    /// Instantaneous current as the device reports it. The unit (some report milliamperes, not the
    /// documented microamperes) and the sign differ between devices, so only the charge counter is trusted.
    pub current_raw: Option<i64>,
    pub temperature_c: f32,
    pub screen_on: bool,
    /// In Doze (deep idle), where network access and wake locks are restricted.
    pub doze: bool,
}

fn int_extra(env: &mut JNIEnv<'_>, intent: &JObject<'_>, key: &str, default: i32) -> jni::errors::Result<i32> {
    let key = env.new_string(key)?;
    env.call_method(intent, "getIntExtra", "(Ljava/lang/String;I)I", &[JValue::Object(&key), JValue::Int(default)])?
        .i()
}

pub fn power_info() -> Option<PowerInfo> {
    with_env(|env, activity| {
        let context = env
            .call_method(activity, "getApplicationContext", "()Landroid/content/Context;", &[])?
            .l()?;

        // ACTION_BATTERY_CHANGED is sticky, so registering no receiver returns the latest state.
        let action = env.new_string("android.intent.action.BATTERY_CHANGED")?;
        let filter = env.new_object("android/content/IntentFilter", "(Ljava/lang/String;)V", &[JValue::Object(&action)])?;
        let battery = env
            .call_method(
                &context,
                "registerReceiver",
                "(Landroid/content/BroadcastReceiver;Landroid/content/IntentFilter;)Landroid/content/Intent;",
                &[JValue::Object(&JObject::null()), JValue::Object(&filter)],
            )?
            .l()?;
        if battery.is_null() {
            return Err(jni::errors::Error::NullPtr("battery intent"));
        }
        let level = int_extra(env, &battery, "level", -1)?;
        let scale = int_extra(env, &battery, "scale", 100)?.max(1);
        let plugged = int_extra(env, &battery, "plugged", 0)?;
        let temperature = int_extra(env, &battery, "temperature", 0)?;

        let manager = system_service(env, &context, "batterymanager")?;
        let mut property = |id: i32| -> jni::errors::Result<Option<i64>> {
            let value = env.call_method(&manager, "getLongProperty", "(I)J", &[JValue::Int(id)])?.j()?;
            // Long.MIN_VALUE means the device does not report it.
            Ok((value != i64::MIN).then_some(value))
        };
        // BATTERY_PROPERTY_CHARGE_COUNTER = 1, BATTERY_PROPERTY_CURRENT_NOW = 2
        let charge_uah = property(1)?.filter(|v| *v > 0);
        let current_raw = property(2)?;

        let power = system_service(env, &context, "power")?;
        let screen_on = env.call_method(&power, "isInteractive", "()Z", &[])?.z()?;
        let doze = env.call_method(&power, "isDeviceIdleMode", "()Z", &[])?.z()?;

        Ok(PowerInfo {
            percent: level * 100 / scale,
            charging: plugged != 0,
            charge_uah,
            current_raw,
            temperature_c: temperature as f32 / 10.0,
            screen_on,
            doze,
        })
    })
}

/// Whether the system is in dark mode. NativeActivity does not report it to winit.
pub fn is_dark_mode() -> Option<bool> {
    with_env(|env, activity| {
        let res = env
            .call_method(activity, "getResources", "()Landroid/content/res/Resources;", &[])?
            .l()?;
        let config = env
            .call_method(&res, "getConfiguration", "()Landroid/content/res/Configuration;", &[])?
            .l()?;
        let mode = env.get_field(&config, "uiMode", "I")?.i()?;
        // UI_MODE_NIGHT_MASK is 0x30 and UI_MODE_NIGHT_YES is 0x20.
        Ok(mode & 0x30 == 0x20)
    })
}

/// Height of the navigation bar (or gesture bar) in physical pixels. NativeActivity draws
/// underneath it as well. Zero until the window is attached.
pub fn navigation_bar_height_px() -> f32 {
    static HEIGHT: OnceLock<f32> = OnceLock::new();
    if let Some(h) = HEIGHT.get() {
        return *h;
    }
    let height = with_env(|env, activity| {
        let window = env.call_method(activity, "getWindow", "()Landroid/view/Window;", &[])?.l()?;
        let decor = env.call_method(&window, "getDecorView", "()Landroid/view/View;", &[])?.l()?;
        let insets = env
            .call_method(&decor, "getRootWindowInsets", "()Landroid/view/WindowInsets;", &[])?
            .l()?;
        if insets.is_null() {
            return Ok(None);
        }
        let mask = env.call_static_method("android/view/WindowInsets$Type", "navigationBars", "()I", &[])?.i()?;
        let bars = env
            .call_method(&insets, "getInsets", "(I)Landroid/graphics/Insets;", &[JValue::Int(mask)])?
            .l()?;
        Ok(Some(env.get_field(&bars, "bottom", "I")?.i()? as f32))
    })
    .flatten();
    match height {
        Some(h) => *HEIGHT.get_or_init(|| h),
        None => 0.0,
    }
}

/// Height of the status bar in physical pixels. NativeActivity draws underneath it,
/// so the UI has to leave this much room at the top.
pub fn status_bar_height_px() -> f32 {
    static HEIGHT: OnceLock<f32> = OnceLock::new();
    *HEIGHT.get_or_init(|| {
        with_env(|env, activity| {
            let res = env
                .call_method(activity, "getResources", "()Landroid/content/res/Resources;", &[])?
                .l()?;
            let name = env.new_string("status_bar_height")?;
            let kind = env.new_string("dimen")?;
            let package = env.new_string("android")?;
            let id = env
                .call_method(
                    &res,
                    "getIdentifier",
                    "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)I",
                    &[JValue::Object(&name), JValue::Object(&kind), JValue::Object(&package)],
                )?
                .i()?;
            if id == 0 {
                return Ok(0.0);
            }
            Ok(env.call_method(&res, "getDimensionPixelSize", "(I)I", &[JValue::Int(id)])?.i()? as f32)
        })
        .unwrap_or(0.0)
    })
}
