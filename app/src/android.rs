//! Android glue: JNI calls into the framework for things NativeActivity lacks.

use std::path::PathBuf;
use std::sync::OnceLock;

use winit::platform::android::activity::AndroidApp;
use jni::objects::{GlobalRef, JObject, JString, JValue};
use jni::{JNIEnv, JavaVM};

static APP: OnceLock<AndroidApp> = OnceLock::new();
static MULTICAST_LOCK: OnceLock<GlobalRef> = OnceLock::new();

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

/// Many devices drop incoming broadcast packets on Wi-Fi unless a multicast lock is held,
/// which would make this device invisible to discovery while receiving.
pub fn acquire_multicast_lock() {
    if MULTICAST_LOCK.get().is_some() {
        return;
    }
    with_env(|env, activity| {
        let context = env
            .call_method(activity, "getApplicationContext", "()Landroid/content/Context;", &[])?
            .l()?;
        let service = env.new_string("wifi")?;
        let wifi = env
            .call_method(
                &context,
                "getSystemService",
                "(Ljava/lang/String;)Ljava/lang/Object;",
                &[JValue::Object(&service)],
            )?
            .l()?;
        let tag = env.new_string("airlynx")?;
        let lock = env
            .call_method(
                &wifi,
                "createMulticastLock",
                "(Ljava/lang/String;)Landroid/net/wifi/WifiManager$MulticastLock;",
                &[JValue::Object(&tag)],
            )?
            .l()?;
        env.call_method(&lock, "acquire", "()V", &[])?;
        let _ = MULTICAST_LOCK.set(env.new_global_ref(lock)?);
        Ok(())
    });
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
