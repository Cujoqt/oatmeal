// End-to-end video probe test — proves the yt-dlp path works against a real
// video on real YouTube.
//
// Ignored by default: it downloads yt-dlp (~30 MB, once, pinned version) and
// talks to YouTube. Run explicitly with:
//
//     cargo test --test e2e_video -- --ignored --nocapture

#[test]
#[ignore = "downloads yt-dlp and contacts YouTube"]
fn probes_a_real_video() {
    // Blender Foundation's "Big Buck Bunny" — Creative Commons, stable, short.
    let url = "https://www.youtube.com/watch?v=YE7VzlLtp-4";
    let info = oatmeal_app_lib::video::probe(url).expect("probe");

    println!("probed: {} ({}s)", info.title, info.duration_secs);

    assert!(
        info.duration_secs > 60.0,
        "unexpected duration {}",
        info.duration_secs
    );
    assert!(!info.title.is_empty(), "expected a non-empty title");
}
