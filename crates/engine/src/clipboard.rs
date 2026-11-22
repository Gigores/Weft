#[cfg(not(target_os = "android"))]
pub use system::{get, set};

#[cfg(not(target_os = "android"))]
mod system {
    use arboard::Clipboard;
    use std::sync::{Mutex, OnceLock};

    static CLIPBOARD: OnceLock<Mutex<Clipboard>> = OnceLock::new();

    /// Returns the clipboard, creating it on first success.
    /// If creation fails, the next call tries again.
    fn clipboard() -> Option<&'static Mutex<Clipboard>> {
        if let Some(c) = CLIPBOARD.get() {
            return Some(c);
        }

        let new = Clipboard::new().ok()?;
        Some(CLIPBOARD.get_or_init(|| Mutex::new(new)))
    }

    pub fn get() -> Option<String> {
        clipboard()?.lock().ok()?.get_text().ok()
    }

    pub fn set(text: &str) -> bool {
        clipboard()
            .and_then(|c| c.lock().ok())
            .map(|mut c| c.set_text(text).is_ok())
            .unwrap_or(false)
    }
}

#[cfg(target_os = "android")]
pub use android::{get, set};

#[cfg(target_os = "android")]
pub(crate) use android::init;

#[cfg(target_os = "android")]
mod android {
    use std::cell::OnceCell;

    use jni::errors::{Error, Result};
    use jni::objects::{JObject, JString, JValue};
    use jni::refs::Global;
    use jni::vm::JavaVM;
    use jni::{Env, jni_sig, jni_str};

    use crate::AndroidApp;

    thread_local! {
        // The NativeActivity and clipboard actions share the game's event loop thread.
        static APP: OnceCell<AndroidApp> = const { OnceCell::new() };
    }

    pub(crate) fn init(app: AndroidApp) {
        APP.with(|cell| {
            let _ = cell.set(app);
        });
    }

    fn with_clipboard<T>(action: impl FnOnce(&mut Env<'_>, &JObject<'_>) -> Result<T>) -> Option<T> {
        let result = APP.with(|cell| {
            let app = cell.get().ok_or(Error::NullPtr("Android activity"))?;
            JavaVM::singleton()?.attach_current_thread(|env| {
                let raw = app.activity_as_ptr() as jni::sys::jobject;
                // SAFETY: The borrowed global reference is valid while app is alive.
                let activity = unsafe { env.as_cast_raw::<Global<JObject>>(&raw)? };
                let service = JString::new(env, "clipboard")?;
                let clipboard = env
                    .call_method(
                        &*activity,
                        jni_str!("getSystemService"),
                        jni_sig!("(Ljava/lang/String;)Ljava/lang/Object;"),
                        &[JValue::Object(service.as_ref())],
                    )?
                    .l()?;
                action(env, &clipboard)
            })
        });
        result
            .map_err(|error| log::warn!("Android clipboard: {error}"))
            .ok()
    }

    pub fn get() -> Option<String> {
        with_clipboard(|env, clipboard| {
            let clip = env
                .call_method(clipboard, jni_str!("getPrimaryClip"), jni_sig!("()Landroid/content/ClipData;"), &[])?
                .l()?;
            if clip.is_null() {
                return Ok(None);
            }
            let item = env
                .call_method(&clip, jni_str!("getItemAt"), jni_sig!("(I)Landroid/content/ClipData$Item;"), &[JValue::Int(0)])?
                .l()?;
            let text = env
                .call_method(&item, jni_str!("getText"), jni_sig!("()Ljava/lang/CharSequence;"), &[])?
                .l()?;
            if text.is_null() {
                return Ok(None);
            }
            let text = env
                .call_method(&text, jni_str!("toString"), jni_sig!("()Ljava/lang/String;"), &[])?
                .l()?;
            Ok(Some(env.cast_local::<JString>(text)?.try_to_string(env)?))
        })
        .flatten()
    }

    pub fn set(text: &str) -> bool {
        with_clipboard(|env, clipboard| {
            let label = JString::new(env, "Weft")?;
            let text = JString::new(env, text)?;
            let clip = env
                .call_static_method(
                    jni_str!("android/content/ClipData"),
                    jni_str!("newPlainText"),
                    jni_sig!("(Ljava/lang/CharSequence;Ljava/lang/CharSequence;)Landroid/content/ClipData;"),
                    &[JValue::Object(label.as_ref()), JValue::Object(text.as_ref())],
                )?
                .l()?;
            env.call_method(clipboard, jni_str!("setPrimaryClip"), jni_sig!("(Landroid/content/ClipData;)V"), &[JValue::Object(&clip)])?;
            Ok(())
        })
        .is_some()
    }
}
