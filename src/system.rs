//! Recording sounds and reversible per-session Windows audio ducking.
#[cfg(target_os = "windows")]
use windows::Win32::{Media::Audio::*, System::Com::*};

#[derive(Default)]
pub struct RecordingEffects {
    #[cfg(target_os = "windows")]
    volumes: Vec<(ISimpleAudioVolume, f32, f32)>,
    #[cfg(target_os = "windows")]
    com_initialized: bool,
    sound: bool,
}

impl RecordingEffects {
    pub fn start(preferences: &crate::config::Preferences) -> Self {
        let mut effects = Self::default();
        effects.sound = preferences.interaction_sounds;
        #[cfg(target_os = "windows")]
        if preferences.duck_audio {
            use windows::core::Interface;
            unsafe {
                // The session thread owns these COM interfaces for the whole take.
                effects.com_initialized = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
                let result = (|| -> windows::core::Result<()> {
                    let enumerator: IMMDeviceEnumerator =
                        CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
                    let device = enumerator.GetDefaultAudioEndpoint(eRender, eMultimedia)?;
                    let manager: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None)?;
                    let sessions = manager.GetSessionEnumerator()?;
                    for index in 0..sessions.GetCount()? {
                        let session = sessions.GetSession(index)?;
                        let control: IAudioSessionControl2 = session.cast()?;
                        if control.GetProcessId()? == std::process::id() {
                            continue;
                        }
                        if let Ok(volume) = session.cast::<ISimpleAudioVolume>() {
                            if let Ok(previous) = volume.GetMasterVolume() {
                                let ducked = previous * 0.2;
                                if volume.SetMasterVolume(ducked, std::ptr::null()).is_ok() {
                                    effects.volumes.push((volume, previous, ducked));
                                }
                            }
                        }
                    }
                    Ok(())
                })();
                if let Err(error) = result {
                    eprintln!("[utterly] audio ducking: {error}");
                }
            }
        }
        if effects.sound {
            play_cue(true);
        }
        effects
    }
}

impl Drop for RecordingEffects {
    fn drop(&mut self) {
        #[cfg(target_os = "windows")]
        for (volume, previous, ducked) in &self.volumes {
            unsafe {
                // Respect volume adjustments the user made during recording.
                if let Ok(current) = volume.GetMasterVolume() {
                    if should_restore(current, *ducked) {
                        let _ = volume.SetMasterVolume(*previous, std::ptr::null());
                    }
                }
            }
        }
        #[cfg(target_os = "windows")]
        {
            self.volumes.clear();
            if self.com_initialized {
                unsafe {
                    CoUninitialize();
                }
            }
        }
        if self.sound {
            play_cue(false);
        }
    }
}

#[cfg(any(target_os = "windows", test))]
fn should_restore(current: f32, ducked: f32) -> bool {
    (current - ducked).abs() < 0.001
}

fn play_cue(start: bool) {
    #[cfg(target_os = "windows")]
    {
        let _ = std::thread::Builder::new()
            .name("utterly-cue".into())
            .stack_size(64 * 1024)
            .spawn(move || {
                #[link(name = "winmm")]
                unsafe extern "system" {
                    fn PlaySoundW(
                        sound: *const u16,
                        module: *mut std::ffi::c_void,
                        flags: u32,
                    ) -> i32;
                }
                let wav: &[u8] = if start {
                    include_bytes!("../assets/record-start.wav")
                } else {
                    include_bytes!("../assets/record-stop.wav")
                };
                // SND_MEMORY | SND_NODEFAULT, synchronous on this short-lived thread.
                unsafe {
                    PlaySoundW(wav.as_ptr().cast(), std::ptr::null_mut(), 0x0004 | 0x0002);
                }
            });
    }
    #[cfg(not(target_os = "windows"))]
    let _ = start;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restore_only_if_user_has_not_adjusted_volume() {
        assert!(should_restore(0.16, 0.16));
        assert!(!should_restore(0.4, 0.16));
        assert!(!should_restore(f32::NAN, 0.16));
    }

    #[cfg(target_os = "windows")]
    #[test]
    #[ignore = "Briefly ducks real Windows audio sessions; run explicitly for local QA"]
    fn native_audio_duck_and_restore() {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok().unwrap();
        }
        let effects = RecordingEffects::start(&crate::config::Preferences {
            duck_audio: true,
            interaction_sounds: false,
            ..Default::default()
        });
        let sessions = effects.volumes.clone();
        assert!(
            !sessions.is_empty(),
            "No other audio sessions available for this test"
        );
        for (volume, _, ducked) in &sessions {
            assert!(should_restore(
                unsafe { volume.GetMasterVolume().unwrap() },
                *ducked
            ));
        }
        drop(effects);
        for (volume, original, _) in &sessions {
            assert!(should_restore(
                unsafe { volume.GetMasterVolume().unwrap() },
                *original
            ));
        }
        println!(
            "Verified duck and restore for {} native audio sessions",
            sessions.len()
        );
        drop(sessions);
        unsafe {
            CoUninitialize();
        }
    }
}
