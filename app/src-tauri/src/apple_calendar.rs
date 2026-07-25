//! Apple Calendar, read through EventKit.
//!
//! Whatever calendars the Mac already has — iCloud, Google, Exchange, local —
//! are visible here without an account, a key, or a network call. The one cost is
//! a macOS permission prompt on first use, driven by `NSCalendarsUsageDescription`
//! in `Info.plist`.
//!
//! EventKit lives in Objective-C, so this module is the app's only unsafe
//! surface. It stays deliberately small: ask for access, fetch a window of
//! events, hand plain structs to the rest of the app.
//!
//! The store must not be a global: EventKit hands out its events on the thread
//! that created the store, and a fresh store per query is cheap next to the
//! Whisper work happening elsewhere.

use std::sync::mpsc;
use std::time::Duration;

use objc2::rc::Retained;
use objc2::runtime::{Bool, NSObjectProtocol};
use objc2_event_kit::{EKAuthorizationStatus, EKEntityType, EKEvent, EKEventStore};
use objc2_foundation::{NSArray, NSDate, NSError, NSString};
use serde::Serialize;

/// How long to wait for the user to answer the permission prompt.
const ACCESS_TIMEOUT: Duration = Duration::from_secs(120);

/// One occurrence, ready for the "Coming up" panel. Times are ISO-8601 local
/// wall-clock strings with no zone suffix, which the webview parses in local time
/// — the same convention the rest of the app uses.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Event {
    pub id: String,
    pub summary: String,
    pub start: String,
    pub end: Option<String>,
    pub all_day: bool,
    pub location: Option<String>,
    /// A join link, when the invite carries one.
    pub link: Option<String>,
    /// Which calendar it came from, so the UI can say "Work" or "Personal".
    pub calendar: Option<String>,
}

/// What the UI gets back. `authorized` false means macOS hasn't granted calendar
/// access yet, which the panel shows as a one-click prompt rather than an error.
#[derive(Debug, Clone, Serialize)]
pub struct CalendarFeed {
    pub authorized: bool,
    /// True once the user has been asked and said no — the UI then points at
    /// System Settings instead of asking again, because macOS won't re-prompt.
    pub denied: bool,
    pub events: Vec<Event>,
}

/// Current TCC state for calendar reads, without prompting.
pub fn authorization() -> EKAuthorizationStatus {
    unsafe { EKEventStore::authorizationStatusForEntityType(EKEntityType::Event) }
}

/// `FullAccess` is the same raw value as the deprecated `Authorized`, so this one
/// arm covers macOS 13 and 14+ alike. Write-only access is useless to us: it
/// cannot read events.
pub fn is_authorized() -> bool {
    matches!(authorization(), EKAuthorizationStatus::FullAccess)
}

fn is_denied() -> bool {
    matches!(
        authorization(),
        EKAuthorizationStatus::Denied | EKAuthorizationStatus::Restricted
    )
}

/// Ask macOS for calendar access, blocking until the user answers. Returns
/// whether access ended up granted.
///
/// EventKit answers on a background queue, so the completion handler sends the
/// result down a channel that this thread waits on. Tauri runs commands off the
/// UI thread, so blocking here doesn't freeze the window.
pub fn request_access() -> Result<bool, String> {
    if is_authorized() {
        return Ok(true);
    }
    if is_denied() {
        return Ok(false);
    }

    let store = unsafe { EKEventStore::new() };
    let (tx, rx) = mpsc::channel::<bool>();

    // EventKit answers on its own queue, so the block owns the sender rather than
    // borrowing anything from this stack frame.
    let handler = block2::RcBlock::new(move |granted: Bool, _err: *mut NSError| {
        let _ = tx.send(granted.as_bool());
    });
    let handler_ptr =
        &*handler as *const block2::DynBlock<dyn Fn(Bool, *mut NSError)> as *mut _;

    unsafe {
        // macOS 14 replaced the combined permission with a read-only "full access
        // to events"; 13 still only has the older selector.
        if store.respondsToSelector(objc2::sel!(requestFullAccessToEventsWithCompletion:)) {
            store.requestFullAccessToEventsWithCompletion(handler_ptr);
        } else {
            #[allow(deprecated)]
            store.requestAccessToEntityType_completion(EKEntityType::Event, handler_ptr);
        }
    }

    match rx.recv_timeout(ACCESS_TIMEOUT) {
        Ok(granted) => Ok(granted),
        Err(_) => Err("timed out waiting for the calendar permission prompt".into()),
    }
}

/// Events from every calendar the Mac knows about, from the start of yesterday to
/// `days` out. EventKit expands recurrence itself, so a weekly standup arrives as
/// individual occurrences.
pub fn list_events(days: u32) -> Result<CalendarFeed, String> {
    if !is_authorized() {
        return Ok(CalendarFeed {
            authorized: false,
            denied: is_denied(),
            events: Vec::new(),
        });
    }

    let events = unsafe {
        let store = EKEventStore::new();
        let start = NSDate::dateWithTimeIntervalSinceNow(-86_400.0);
        let end = NSDate::dateWithTimeIntervalSinceNow(86_400.0 * f64::from(days));
        let predicate =
            store.predicateForEventsWithStartDate_endDate_calendars(&start, &end, None);
        let matching: Retained<NSArray<EKEvent>> = store.eventsMatchingPredicate(&predicate);

        let mut out = Vec::with_capacity(matching.len());
        for event in matching.iter() {
            if let Some(mapped) = map_event(&event) {
                out.push(mapped);
            }
        }
        out
    };

    Ok(CalendarFeed {
        authorized: true,
        denied: false,
        events,
    })
}

/// Turn one `EKEvent` into a plain struct. An event with no start date is
/// unusable, so it's dropped rather than guessed at.
unsafe fn map_event(event: &EKEvent) -> Option<Event> {
    let all_day = event.isAllDay();
    let start = iso_local(&event.startDate(), all_day);
    let title = event.title().to_string();

    Some(Event {
        id: event
            .eventIdentifier()
            .map(|s| s.to_string())
            .unwrap_or_default(),
        summary: if title.trim().is_empty() {
            "(no title)".into()
        } else {
            title
        },
        start,
        end: Some(iso_local(&event.endDate(), all_day)),
        all_day,
        location: event
            .location()
            .map(|s| s.to_string())
            .filter(|s| !s.trim().is_empty()),
        link: join_link(event),
        calendar: event.calendar().and_then(|c| {
            let name = c.title().to_string();
            (!name.trim().is_empty()).then_some(name)
        }),
    })
}

/// The join link: the event's URL field when it's an http(s) one, else the first
/// link found in the notes — which is where Zoom and Teams invites put it.
unsafe fn join_link(event: &EKEvent) -> Option<String> {
    if let Some(url) = event.URL() {
        let text = url
            .absoluteString()
            .map(|s| s.to_string())
            .unwrap_or_default();
        if text.starts_with("http://") || text.starts_with("https://") {
            return Some(text);
        }
    }
    let notes = event.notes()?.to_string();
    notes
        .split_whitespace()
        .find(|word| word.starts_with("https://"))
        .map(|word| word.trim_end_matches(['>', ')', ',', '.']).to_string())
}

/// Format an `NSDate` as local wall-clock ISO-8601 — `YYYY-MM-DDTHH:MM:SS`, or a
/// bare `YYYY-MM-DD` for all-day events, matching what the webview expects.
unsafe fn iso_local(date: &NSDate, all_day: bool) -> String {
    let formatter = objc2_foundation::NSDateFormatter::new();
    let pattern = if all_day {
        "yyyy-MM-dd"
    } else {
        "yyyy-MM-dd'T'HH:mm:ss"
    };
    formatter.setDateFormat(Some(&NSString::from_str(pattern)));
    formatter.setTimeZone(Some(&objc2_foundation::NSTimeZone::localTimeZone()));
    formatter.stringFromDate(date).to_string()
}
