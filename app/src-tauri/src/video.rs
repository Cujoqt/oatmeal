//! YouTube as a source for a note: fetch a video's audio, transcribe the
//! stretch the user watched, and file it beside the meeting's own transcript.

/// The stretch of a video to transcribe, in seconds from its start.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Range {
    pub start_secs: f64,
    pub end_secs: f64,
}

/// The eleven-character video id out of any YouTube URL shape.
///
/// Parsed by hand rather than with a URL crate: the shapes are few and fixed,
/// and a dependency to split three query strings would not pay for itself.
pub fn video_id(url: &str) -> Result<String, String> {
    let url = url.trim();
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host.strip_prefix("www.").unwrap_or(host);
    let host = host.strip_prefix("m.").unwrap_or(host);

    let raw = match host {
        "youtu.be" => path.split(['?', '&', '/']).next().unwrap_or(""),
        "youtube.com" => {
            if let Some(embed) = path.strip_prefix("embed/") {
                embed.split(['?', '&', '/']).next().unwrap_or("")
            } else {
                let (route, query) = path.split_once('?').unwrap_or((path, ""));
                if route != "watch" {
                    return Err("that isn't a YouTube video link".into());
                }
                query
                    .split('&')
                    .find_map(|pair| pair.strip_prefix("v="))
                    .unwrap_or("")
            }
        }
        _ => return Err("Oatmeal can only read YouTube links".into()),
    };

    // YouTube ids are exactly 11 characters of URL-safe base64. Checking the
    // shape here means a mangled paste fails instantly rather than after a
    // network round trip that reports something less specific.
    let ok = raw.len() == 11
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if !ok {
        return Err("that link doesn't contain a video id".into());
    }
    Ok(raw.to_string())
}

/// `90`, `12:30` or `1:05:20` to seconds.
pub fn parse_timestamp(s: &str) -> Result<f64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty timestamp".into());
    }
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() > 3 {
        return Err(format!("{s} isn't a time"));
    }
    let mut nums = Vec::with_capacity(parts.len());
    for p in &parts {
        let n: f64 = p
            .parse()
            .map_err(|_| format!("{s} isn't a time — use 12:30 or 1:05:20"))?;
        if n < 0.0 {
            return Err(format!("{s} isn't a time"));
        }
        nums.push(n);
    }
    // Only the leading field may exceed 59: 90 means ninety seconds, but 1:70
    // is a typo for 2:10 and guessing which was meant is worse than asking.
    if nums.len() > 1 && nums[1..].iter().any(|n| *n >= 60.0) {
        return Err(format!("{s} isn't a time — minutes and seconds run 0 to 59"));
    }
    Ok(match nums.as_slice() {
        [s] => *s,
        [m, s] => m * 60.0 + s,
        [h, m, s] => h * 3600.0 + m * 60.0 + s,
        _ => unreachable!("length checked above"),
    })
}

/// The shortest stretch worth loading Whisper for.
const MIN_RANGE_SECS: f64 = 1.0;

/// Turn what the user typed into a range inside a video of `duration_secs`.
/// Blank start means the beginning; blank end means run to the end.
pub fn parse_range(start: &str, end: &str, duration_secs: f64) -> Result<Range, String> {
    let start_secs = if start.trim().is_empty() {
        0.0
    } else {
        parse_timestamp(start)?
    };
    let end_secs = if end.trim().is_empty() {
        duration_secs
    } else {
        parse_timestamp(end)?
    };

    if start_secs >= duration_secs {
        return Err(format!(
            "that video is only {} long — the start time is past the end of it",
            human(duration_secs)
        ));
    }
    // Clamped, not rejected: rounding the end up past a video's last second is
    // a reasonable thing to type, and reading past the samples would panic.
    let end_secs = end_secs.min(duration_secs);
    if end_secs - start_secs < MIN_RANGE_SECS {
        return Err("that range is empty — the end time must come after the start".into());
    }
    Ok(Range {
        start_secs,
        end_secs,
    })
}

/// `1:05:20` / `12:30` for error copy.
fn human(secs: f64) -> String {
    let total = secs.max(0.0) as u64;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_id_out_of_every_youtube_url_shape() {
        for url in [
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://youtube.com/watch?v=dQw4w9WgXcQ",
            "http://www.youtube.com/watch?v=dQw4w9WgXcQ&list=PL123&index=2",
            "https://www.youtube.com/watch?list=PL123&v=dQw4w9WgXcQ",
            "https://youtu.be/dQw4w9WgXcQ",
            "https://youtu.be/dQw4w9WgXcQ?t=90",
            "https://www.youtube.com/embed/dQw4w9WgXcQ",
            "https://m.youtube.com/watch?v=dQw4w9WgXcQ",
            "  https://youtu.be/dQw4w9WgXcQ  ",
        ] {
            assert_eq!(video_id(url).unwrap(), "dQw4w9WgXcQ", "failed on {url}");
        }
    }

    #[test]
    fn refuses_anything_that_is_not_a_youtube_video() {
        for url in [
            "",
            "not a url",
            "https://example.com/watch?v=dQw4w9WgXcQ",
            "https://www.youtube.com/",
            "https://www.youtube.com/watch?v=",
            "https://vimeo.com/12345",
            // A channel or playlist is not a video. The spec puts both out of scope.
            "https://www.youtube.com/playlist?list=PL123",
            "https://www.youtube.com/@someone",
        ] {
            assert!(video_id(url).is_err(), "should have rejected {url}");
        }
    }

    #[test]
    fn parses_the_timestamp_shapes_a_person_actually_types() {
        assert_eq!(parse_timestamp("0").unwrap(), 0.0);
        assert_eq!(parse_timestamp("90").unwrap(), 90.0);
        assert_eq!(parse_timestamp("12:30").unwrap(), 750.0);
        assert_eq!(parse_timestamp("1:05:20").unwrap(), 3920.0);
        assert_eq!(parse_timestamp("01:05:20").unwrap(), 3920.0);
        assert_eq!(parse_timestamp(" 12:30 ").unwrap(), 750.0);
    }

    #[test]
    fn refuses_malformed_timestamps() {
        for s in ["", "abc", "12:", ":30", "1:2:3:4", "-5", "12:60", "1:70:00", "12.5.6"] {
            assert!(parse_timestamp(s).is_err(), "should have rejected {s:?}");
        }
    }

    #[test]
    fn an_empty_range_means_the_whole_video() {
        let r = parse_range("", "", 600.0).unwrap();
        assert_eq!(r.start_secs, 0.0);
        assert_eq!(r.end_secs, 600.0);
    }

    #[test]
    fn a_blank_end_runs_to_the_end_of_the_video() {
        let r = parse_range("2:00", "", 600.0).unwrap();
        assert_eq!(r.start_secs, 120.0);
        assert_eq!(r.end_secs, 600.0);
    }

    #[test]
    fn an_end_past_the_duration_is_clamped_not_rejected() {
        // Typing 10:00 for a 9:30 video is a rounding-up, not a mistake worth
        // an error. Reading past the end of the samples would panic.
        let r = parse_range("0:00", "10:00", 570.0).unwrap();
        assert_eq!(r.end_secs, 570.0);
    }

    #[test]
    fn refuses_a_range_that_cannot_describe_anything() {
        // Start past the end of the video is a different video, or a typo. It
        // is the wrong-ten-minutes bug, so it is an error, not a clamp.
        assert!(parse_range("20:00", "25:00", 600.0).is_err());
        assert!(parse_range("5:00", "2:00", 600.0).is_err());
        assert!(parse_range("5:00", "5:00", 600.0).is_err());
        // Under a second of audio is not worth loading Whisper for.
        assert!(parse_range("5:00", "5:00.5", 600.0).is_err());
    }
}
