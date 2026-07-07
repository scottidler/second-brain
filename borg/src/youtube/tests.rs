use super::*;
use std::process::Command;

#[test]
fn test_clean_vtt_removes_headers() {
    let vtt = "WEBVTT\nKind: captions\nLanguage: en\n\n00:00:00.000 --> 00:00:05.000\nHello world\n\n00:00:05.000 --> 00:00:10.000\nThis is a test";
    let result = clean_vtt(vtt);
    assert_eq!(result, "Hello world This is a test");
}

#[test]
fn test_clean_vtt_removes_html_tags() {
    let vtt = "00:00:00.000 --> 00:00:05.000\n<c>Hello</c> <i>world</i>";
    let result = clean_vtt(vtt);
    assert_eq!(result, "Hello world");
}

#[test]
fn test_clean_vtt_deduplicates() {
    let vtt = "00:00:00.000 --> 00:00:05.000\nHello\n\n00:00:05.000 --> 00:00:10.000\nHello\n\n00:00:10.000 --> 00:00:15.000\nWorld";
    let result = clean_vtt(vtt);
    assert_eq!(result, "Hello World");
}

#[test]
fn test_clean_vtt_rolling_prefix_collapses_extensions() {
    // Auto-generated VTT often emits "hello", then "hello world",
    // then "hello world how are you" - each cue extending the previous.
    // Rolling-prefix dedupe collapses these into one line.
    let vtt = "00:00:00.000 --> 00:00:01.000\nhello\n\n00:00:01.000 --> 00:00:02.000\nhello world\n\n00:00:02.000 --> 00:00:03.000\nhello world how are you";
    let result = clean_vtt(vtt);
    assert_eq!(result, "hello world how are you");
}

#[test]
fn test_clean_vtt_rolling_prefix_skips_regressions() {
    // If a later cue is a prefix of the previously accumulated line
    // (silence-fill or partial frame) it should be skipped.
    let vtt = "00:00:00.000 --> 00:00:01.000\nhello world how are you\n\n00:00:01.000 --> 00:00:02.000\nhello world";
    let result = clean_vtt(vtt);
    assert_eq!(result, "hello world how are you");
}

#[test]
fn test_parse_vtt_segments_extracts_start_and_text() {
    let vtt = "WEBVTT\nKind: captions\nLanguage: en\n\n00:00:00.000 --> 00:00:05.000\nHello world\n\n00:00:05.000 --> 00:00:12.500\nthis is the second cue\n\n00:01:30.000 --> 00:01:35.000\nmuch later\n";
    let segs = parse_vtt_segments(vtt);
    assert_eq!(segs.len(), 3);
    assert_eq!(segs[0].0, 0.0);
    assert_eq!(segs[0].1, "Hello world");
    assert_eq!(segs[1].0, 5.0);
    assert_eq!(segs[1].1, "this is the second cue");
    assert_eq!(segs[2].0, 90.0);
    assert_eq!(segs[2].1, "much later");
}

#[test]
fn test_parse_vtt_segments_strips_html_tags() {
    let vtt = "00:00:00.000 --> 00:00:02.000\n<c>tagged</c> <i>text</i>";
    let segs = parse_vtt_segments(vtt);
    assert_eq!(segs.len(), 1);
    assert_eq!(segs[0].1, "tagged text");
}

#[test]
fn test_parse_vtt_segments_dedupes_consecutive_duplicates() {
    let vtt = "00:00:00.000 --> 00:00:02.000\nsame\n\n00:00:02.000 --> 00:00:04.000\nsame\n";
    let segs = parse_vtt_segments(vtt);
    assert_eq!(segs.len(), 1);
    assert_eq!(segs[0].1, "same");
}

#[test]
fn test_parse_vtt_segments_strips_classed_tags() {
    // Auto-generated captions wrap each word in a classed span, not the
    // bare `<c>`/`</c>` the old literal-string replace handled.
    let vtt = "00:00:00.000 --> 00:00:02.000\n<c.colorE5E5E5>tagged</c> plain";
    let segs = parse_vtt_segments(vtt);
    assert_eq!(segs.len(), 1);
    assert_eq!(segs[0].1, "tagged plain");
    assert!(!segs[0].1.contains("<c"), "classed tag leaked: {}", segs[0].1);
}

#[test]
fn test_parse_vtt_segments_strips_timing_tags() {
    // Rolling captions stamp per-word timing tags inside a growing cue.
    let vtt = "00:00:00.000 --> 00:00:02.000\nwelcome<00:00:00.500> back<00:00:01.000> home";
    let segs = parse_vtt_segments(vtt);
    assert_eq!(segs.len(), 1);
    assert_eq!(segs[0].1, "welcome back home");
    assert!(!segs[0].1.contains('<'), "timing tag leaked: {}", segs[0].1);
}

/// Negative case proving the compounding bug is fixed: a segment carrying
/// an untouched classed or timing tag would fail this on the raw literal
/// forms the old `.replace("<c>", "")` string-replace missed entirely.
#[test]
fn test_parse_vtt_segments_no_tag_substrings_survive() {
    let vtt = "00:00:00.000 --> 00:00:02.000\n<c.colorE5E5E5>hello</c> world<00:00:01.500><c> today</c>";
    let segs = parse_vtt_segments(vtt);
    assert_eq!(segs.len(), 1);
    assert_eq!(segs[0].1, "hello world today");
    for (_, text) in &segs {
        assert!(!text.contains("<c"), "classed tag survived: {text}");
        assert!(!text.contains('<'), "timing tag survived: {text}");
    }
}

/// Fixture modeled on a real YouTube rolling auto-caption VTT: each settling
/// line is followed by a "collapse" cue repeating it verbatim, then the next
/// cue re-emits the settled line joined with a growing continuation carrying
/// classed and per-word timing tags. Before the fix this produced every
/// spoken line twice; the rolling-overlap collapse must yield each once.
#[test]
fn test_parse_vtt_segments_rolling_caption_yields_each_line_once() {
    let vtt = concat!(
        "WEBVTT\n",
        "Kind: captions\n",
        "Language: en\n",
        "\n",
        "00:00:00.080 --> 00:00:02.780 align:start position:0%\n",
        "welcome<00:00:00.560><c> back</c><c> to</c><c> the</c><c> channel</c>\n",
        "\n",
        "00:00:02.780 --> 00:00:02.790 align:start position:0%\n",
        "welcome back to the channel\n",
        "\n",
        "00:00:02.790 --> 00:00:05.500 align:start position:0%\n",
        "welcome back to the channel\n",
        "everyone<00:00:03.200><c> today</c><c> we're</c>\n",
        "\n",
        "00:00:05.500 --> 00:00:05.510 align:start position:0%\n",
        "welcome back to the channel everyone today we're\n",
        "\n",
        "00:00:05.510 --> 00:00:08.000 align:start position:0%\n",
        "welcome back to the channel everyone today we're\n",
        "going<00:00:06.000><c> to</c><c> talk</c><c> about</c><c> rust</c>\n",
        "\n",
        "00:00:08.000 --> 00:00:08.010 align:start position:0%\n",
        "welcome back to the channel everyone today we're going to talk about rust\n",
    );
    let segs = parse_vtt_segments(vtt);
    assert_eq!(
        segs.len(),
        1,
        "rolling caption should collapse to one segment, got {segs:?}"
    );
    assert_eq!(
        segs[0].1,
        "welcome back to the channel everyone today we're going to talk about rust"
    );
    // Earliest cue's start time is kept as the line grows.
    assert_eq!(segs[0].0, 0.08);
    assert!(!segs[0].1.contains("<c"), "classed tag leaked: {}", segs[0].1);
    assert!(!segs[0].1.contains('<'), "timing tag leaked: {}", segs[0].1);
}

#[test]
fn test_clean_vtt_rolling_prefix_keeps_distinct_lines() {
    let vtt = "00:00:00.000 --> 00:00:01.000\nhello world\n\n00:00:01.000 --> 00:00:02.000\nhow are you\n\n00:00:02.000 --> 00:00:03.000\nhow are you doing today";
    let result = clean_vtt(vtt);
    assert_eq!(result, "hello world how are you doing today");
}

#[test]
fn test_extract_video_id_watch() {
    let id = extract_video_id("https://www.youtube.com/watch?v=dQw4w9WgXcQ");
    assert_eq!(id, Some("dQw4w9WgXcQ".to_string()));
}

#[test]
fn test_extract_video_id_short() {
    let id = extract_video_id("https://youtu.be/dQw4w9WgXcQ");
    assert_eq!(id, Some("dQw4w9WgXcQ".to_string()));
}

#[test]
fn test_extract_video_id_shorts() {
    let id = extract_video_id("https://youtube.com/shorts/dQw4w9WgXcQ");
    assert_eq!(id, Some("dQw4w9WgXcQ".to_string()));
}

#[test]
fn test_extract_video_id_none() {
    let id = extract_video_id("https://example.com/page");
    assert_eq!(id, None);
}

#[test]
fn test_generate_embed_code() {
    let code = generate_embed_code("abc123_-XYZ", 854, 480);
    assert!(code.contains("abc123_-XYZ"));
    assert!(code.contains("854"));
    assert!(code.contains("480"));
    assert!(code.contains("iframe"));
}

#[test]
fn test_frame_budget_short_video() {
    let cfg = YoutubeSlidesConfig::default();
    let (budget, fps) = frame_budget(15.0, &cfg);
    assert_eq!(budget, 30);
    assert!((fps - 1.0).abs() < 1e-3, "expected ~1 fps for short video, got {fps}");
}

#[test]
fn test_frame_budget_one_minute() {
    let cfg = YoutubeSlidesConfig::default();
    let (budget, fps) = frame_budget(60.0, &cfg);
    assert_eq!(budget, 40);
    // 40 / 60 = 0.666...
    assert!(
        (fps - 0.6667).abs() < 1e-3,
        "expected ~0.667 fps for 60s video, got {fps}"
    );
}

#[test]
fn test_frame_budget_three_minutes() {
    let cfg = YoutubeSlidesConfig::default();
    let (budget, fps) = frame_budget(180.0, &cfg);
    assert_eq!(budget, 60);
    // 60 / 180 = 0.333...
    assert!(
        (fps - 0.3333).abs() < 1e-3,
        "expected ~0.333 fps for 180s video, got {fps}"
    );
}

#[test]
fn test_frame_budget_long_video_caps_at_100() {
    let cfg = YoutubeSlidesConfig::default();
    let (budget, _) = frame_budget(3600.0, &cfg);
    assert_eq!(budget, 100);
}

#[test]
fn test_frame_budget_respects_max_fps() {
    let cfg = YoutubeSlidesConfig {
        max_fps: 0.5,
        ..YoutubeSlidesConfig::default()
    };
    let (_, fps) = frame_budget(15.0, &cfg);
    assert!(fps <= 0.5, "fps should be capped at max_fps; got {fps}");
}

#[test]
fn test_frame_budget_respects_max_frames() {
    let cfg = YoutubeSlidesConfig {
        max_frames: 50,
        ..YoutubeSlidesConfig::default()
    };
    let (budget, _) = frame_budget(3600.0, &cfg);
    assert_eq!(budget, 50);
}

#[tokio::test]
async fn test_extract_frames_disabled_returns_empty() {
    let cfg = YoutubeSlidesConfig {
        enabled: false,
        ..YoutubeSlidesConfig::default()
    };
    let tmp = std::env::temp_dir().join("borg-test-frames-disabled");
    let _ = std::fs::remove_dir_all(&tmp);
    let thread_args = [
        "-threads".to_string(),
        "2".to_string(),
        "-filter_threads".to_string(),
        "2".to_string(),
    ];
    let frames = extract_frames(
        Path::new("/nonexistent.mp4"),
        &tmp.join("frames"),
        30.0,
        &cfg,
        &thread_args,
        600,
    )
    .await
    .expect("disabled path should not error");
    assert!(frames.is_empty());
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Synthesize a small test mp4 with `ffmpeg -f lavfi` and run frame extraction.
/// Skipped if ffmpeg is not on PATH; serves as a smoke test that the filter
/// chain is well-formed and the sidecar gets written.
#[tokio::test]
async fn test_extract_frames_synthetic_video() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("ffmpeg not found; skipping test_extract_frames_synthetic_video");
        return;
    }
    let tmp = std::env::temp_dir().join("borg-test-frames-synth");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("create tmp");
    let video = tmp.join("synthetic.mp4");

    // 10s of testsrc at 5fps, 320x240. Plenty of motion so mpdecimate
    // does not collapse it to nothing.
    let synth = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=10:rate=5:size=320x240",
            "-pix_fmt",
            "yuv420p",
            &video.to_string_lossy(),
        ])
        .output()
        .expect("ffmpeg synth");
    assert!(
        synth.status.success(),
        "ffmpeg synth: {:?}",
        String::from_utf8_lossy(&synth.stderr)
    );

    let cfg = YoutubeSlidesConfig::default();
    let frames_dir = tmp.join("frames");
    let thread_args = [
        "-threads".to_string(),
        "2".to_string(),
        "-filter_threads".to_string(),
        "2".to_string(),
    ];
    let frames = extract_frames(&video, &frames_dir, 10.0, &cfg, &thread_args, 600)
        .await
        .expect("extract_frames");

    assert!(
        !frames.is_empty(),
        "expected at least one frame from a 10s testsrc video"
    );
    // Auto-fps for 10s video is 1fps from the table; mpdecimate will not collapse
    // testsrc's continuously-changing pattern. Expect close to budget=30.
    assert!(
        frames.len() <= 30,
        "frames {} should not exceed budget=30",
        frames.len(),
    );

    // Frames should be named frame_NNNN.jpg, sequentially indexed.
    for (i, fr) in frames.iter().enumerate() {
        let expected = format!("frame_{:04}.jpg", i + 1);
        assert_eq!(fr.path.file_name().and_then(|s| s.to_str()), Some(expected.as_str()),);
        assert!(fr.path.exists(), "frame should exist on disk: {}", fr.path.display());
    }

    // Sidecar was written.
    let sidecar_path = tmp.join("frames.yml");
    assert!(sidecar_path.exists(), "frames.yml sidecar should exist");
    let sidecar_yaml = std::fs::read_to_string(&sidecar_path).expect("read sidecar");
    let sidecar: FramesSidecar = serde_yaml::from_str(&sidecar_yaml).expect("parse sidecar");
    assert_eq!(sidecar.frames_extracted as usize, frames.len());
    assert_eq!(sidecar.video_duration_secs, 10.0);

    let _ = std::fs::remove_dir_all(&tmp);
}
