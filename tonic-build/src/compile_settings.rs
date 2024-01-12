use std::{
    marker::PhantomData,
    mem::take,
    sync::{Mutex, MutexGuard},
};

#[derive(Debug, Clone)]
pub(crate) struct CompileSettings {
    pub(crate) codec_path: String,
}

impl Default for CompileSettings {
    fn default() -> Self {
        Self {
            codec_path: "tonic::codec::ProstCodec".to_string(),
        }
    }
}

thread_local! {
    static COMPILE_SETTINGS: Mutex<Option<CompileSettings>> = Default::default();
}

/// Called before compile, this installs a CompileSettings in the current thread's
/// context, so that live code generation can access the settings.
/// The previous state is restored when you drop the SettingsGuard.
pub(crate) fn set_context(new_settings: CompileSettings) -> SettingsGuard {
    COMPILE_SETTINGS.with(|settings| {
        let mut guard = settings
            .lock()
            .expect("threadlocal mutex should always succeed");
        let old_settings = guard.clone();
        *guard = Some(new_settings);
        SettingsGuard {
            previous_settings: old_settings,
            _pd: PhantomData,
        }
    })
}

/// Access the current compile settings. This is populated only during
/// code generation compile() or compile_with_config() time.
pub(crate) fn load() -> CompileSettings {
    COMPILE_SETTINGS.with(|settings| {
        settings
            .lock()
            .expect("threadlocal mutex should always succeed")
            .clone()
            .unwrap_or_default()
    })
}

type PhantomUnsend = PhantomData<MutexGuard<'static, ()>>;

pub(crate) struct SettingsGuard {
    previous_settings: Option<CompileSettings>,
    _pd: PhantomUnsend,
}

impl Drop for SettingsGuard {
    fn drop(&mut self) {
        COMPILE_SETTINGS.with(|settings| {
            let mut guard = settings
                .lock()
                .expect("threadlocal mutex should always succeed");
            *guard = take(&mut self.previous_settings);
        })
    }
}
