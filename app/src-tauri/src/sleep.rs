//! Sleep is a hole in the recording.
//!
//! macOS suspends the audio devices when the machine sleeps — a closed lid, an
//! idle timeout — and nothing reaches either capture lane until it wakes. The
//! session itself knows nothing about that: the recorders are still "running",
//! so the meeting quietly loses however long the machine was out, and the
//! transcript reads as though the room went silent.
//!
//! NSWorkspace posts the two notifications that make this visible. The app
//! watches for them so that it can stop the take on wake and say what happened,
//! rather than leaving a gap in the middle of one recording.

#[cfg(target_os = "macos")]
mod imp {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::Instant;

    use block2::RcBlock;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use objc2_foundation::NSString;

    /// Process start, so a sleep instant can live in an `AtomicU64` instead of a
    /// lock: the notifications arrive on AppKit's thread, and the callback runs
    /// straight from them.
    fn epoch() -> &'static Instant {
        static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        EPOCH.get_or_init(Instant::now)
    }

    fn now_ms() -> u64 {
        epoch().elapsed().as_millis() as u64
    }

    /// Call `on_wake` with the sleep duration each time the machine wakes.
    ///
    /// The observers are registered for the life of the process — there is
    /// nothing to unregister them for, and dropping the blocks while AppKit
    /// still holds them would be a use-after-free.
    pub fn on_wake<F>(on_wake: F)
    where
        F: Fn(u64) + Send + Sync + 'static,
    {
        let slept_at = Arc::new(AtomicU64::new(0));
        epoch();

        let sleeping = slept_at.clone();
        let will_sleep = RcBlock::new(move |_note: *mut AnyObject| {
            sleeping.store(now_ms(), Ordering::SeqCst);
        });

        let woke = slept_at.clone();
        let did_wake = RcBlock::new(move |_note: *mut AnyObject| {
            let at = woke.swap(0, Ordering::SeqCst);
            // A wake with no sleep behind it (the machine woke for something
            // else, or the app launched between the two) has no gap to report.
            if at == 0 {
                return;
            }
            on_wake(now_ms().saturating_sub(at));
        });

        unsafe {
            let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
            let center: *mut AnyObject = msg_send![workspace, notificationCenter];
            let nil = std::ptr::null_mut::<AnyObject>();

            for (name, block) in [
                ("NSWorkspaceWillSleepNotification", &will_sleep),
                ("NSWorkspaceDidWakeNotification", &did_wake),
            ] {
                let name = NSString::from_str(name);
                let _: *mut AnyObject = msg_send![
                    center,
                    addObserverForName: &*name,
                    object: nil,
                    queue: nil,
                    usingBlock: &**block,
                ];
            }
        }

        // AppKit holds the blocks now; they must outlive this call.
        std::mem::forget(will_sleep);
        std::mem::forget(did_wake);
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    pub fn on_wake<F>(_on_wake: F)
    where
        F: Fn(u64) + Send + Sync + 'static,
    {
    }
}

pub use imp::on_wake;
