//! Is the Mac playing audio? (So the figure can dance to your music.)
//!
//! Asks CoreAudio whether the default output device is running "somewhere"
//! (any app using it), once a second on a background thread. Our own sound
//! effects also run the device, so the caller ignores the signal shortly after
//! playing one.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[repr(C)]
struct AudioObjectPropertyAddress {
    selector: u32,
    scope: u32,
    element: u32,
}

#[link(name = "CoreMediaIO", kind = "framework")]
extern "C" {
    fn CMIOObjectGetPropertyDataSize(
        object: u32,
        address: *const AudioObjectPropertyAddress,
        qualifier_size: u32,
        qualifier: *const std::ffi::c_void,
        size: *mut u32,
    ) -> i32;
    fn CMIOObjectGetPropertyData(
        object: u32,
        address: *const AudioObjectPropertyAddress,
        qualifier_size: u32,
        qualifier: *const std::ffi::c_void,
        size: u32,
        used: *mut u32,
        data: *mut std::ffi::c_void,
    ) -> i32;
}

const CMIO_DEVICES: u32 = four_cc(b"dev#");

/// Whether any app is using a camera (a video call, most likely). Doesn't
/// open the camera: it asks CoreMediaIO whether each device is running.
pub fn camera_in_use() -> bool {
    let addr = AudioObjectPropertyAddress {
        selector: CMIO_DEVICES,
        scope: SCOPE_GLOBAL,
        element: ELEMENT_MAIN,
    };
    let mut size = 0u32;
    // SAFETY: read-only CoreMediaIO queries with valid addresses and
    // out-parameters sized as reported; device ids are u32 (CMIOObjectID).
    unsafe {
        if CMIOObjectGetPropertyDataSize(SYSTEM_OBJECT, &addr, 0, std::ptr::null(), &mut size) != 0
        {
            return false;
        }
        let n = size as usize / std::mem::size_of::<u32>();
        let mut ids = vec![0u32; n];
        let mut used = 0u32;
        if CMIOObjectGetPropertyData(
            SYSTEM_OBJECT,
            &addr,
            0,
            std::ptr::null(),
            size,
            &mut used,
            ids.as_mut_ptr() as *mut std::ffi::c_void,
        ) != 0
        {
            return false;
        }
        ids.into_iter().any(|id| {
            let running = AudioObjectPropertyAddress {
                selector: IS_RUNNING_SOMEWHERE,
                scope: SCOPE_GLOBAL,
                element: ELEMENT_MAIN,
            };
            let mut v = 0u32;
            let mut used = 0u32;
            CMIOObjectGetPropertyData(
                id,
                &running,
                0,
                std::ptr::null(),
                4,
                &mut used,
                &mut v as *mut u32 as *mut std::ffi::c_void,
            ) == 0
                && v != 0
        })
    }
}

#[link(name = "CoreAudio", kind = "framework")]
extern "C" {
    fn AudioObjectGetPropertyData(
        object: u32,
        address: *const AudioObjectPropertyAddress,
        qualifier_size: u32,
        qualifier: *const std::ffi::c_void,
        size: *mut u32,
        data: *mut std::ffi::c_void,
    ) -> i32;
}

const fn four_cc(s: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*s)
}

const SYSTEM_OBJECT: u32 = 1;
const DEFAULT_OUTPUT: u32 = four_cc(b"dOut");
const IS_RUNNING_SOMEWHERE: u32 = four_cc(b"gone");
const SCOPE_GLOBAL: u32 = four_cc(b"glob");
const ELEMENT_MAIN: u32 = 0;

fn get_u32(object: u32, selector: u32) -> Option<u32> {
    let addr = AudioObjectPropertyAddress {
        selector,
        scope: SCOPE_GLOBAL,
        element: ELEMENT_MAIN,
    };
    let mut value: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: a read-only CoreAudio query with a valid address struct and a
    // correctly sized out-parameter; both properties are UInt32-sized
    // (AudioObjectID and UInt32).
    let err = unsafe {
        AudioObjectGetPropertyData(
            object,
            &addr,
            0,
            std::ptr::null(),
            &mut size,
            &mut value as *mut u32 as *mut std::ffi::c_void,
        )
    };
    (err == 0).then_some(value)
}

/// Whether any app is playing through the default output right now.
pub fn output_in_use() -> bool {
    get_u32(SYSTEM_OBJECT, DEFAULT_OUTPUT)
        .and_then(|dev| get_u32(dev, IS_RUNNING_SOMEWHERE))
        .is_some_and(|v| v != 0)
}

/// What's going on around the user, polled in the background.
#[derive(Clone, Default)]
pub struct Presence {
    /// Some app is playing audio.
    pub music: Arc<AtomicBool>,
    /// Some app is using a camera.
    pub camera: Arc<AtomicBool>,
}

/// Poll audio every second and the camera every two, on a background thread.
pub fn spawn_watcher() -> Presence {
    let p = Presence::default();
    let out = p.clone();
    let spawned = std::thread::Builder::new()
        .name("stickman-presence".into())
        .spawn(move || {
            let mut tick = 0u32;
            loop {
                out.music.store(output_in_use(), Ordering::Relaxed);
                if tick % 2 == 0 {
                    out.camera.store(camera_in_use(), Ordering::Relaxed);
                }
                tick = tick.wrapping_add(1);
                std::thread::sleep(Duration::from_secs(1));
            }
        });
    if spawned.is_err() {
        log::warn!("presence: could not start the watcher");
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_char_codes() {
        assert_eq!(DEFAULT_OUTPUT, 0x644F_7574);
        assert_eq!(SCOPE_GLOBAL, 0x676C_6F62);
    }

    #[test]
    fn query_does_not_crash() {
        let _ = output_in_use();
        let _ = camera_in_use();
    }
}
