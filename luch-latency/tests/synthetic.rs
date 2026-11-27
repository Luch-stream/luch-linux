//! The analyser against a synthetic camera whose every latency is known (src/synth.rs): it must find the true
//! latency within one camera frame, through blur, noise, perspective, a rolling shutter, tearing, dropped frames,
//! jitter, slow panels and a TV with its own refresh.

use luch_latency::analyze::{analyze, Options, Summary};
use luch_latency::geom::pt;
use luch_latency::synth::{Camera, Scene};
use luch_latency::video::{Frame, Source};

/// Renders the frames once: the analyser plays a source twice.
struct Recorded {
    frames: Vec<Frame>,
}

impl Recorded {
    fn new(cam: &Camera, time_scale: f64) -> Recorded {
        let s = cam.scene();
        let frames = (0..cam.frame_count())
            .map(|k| Frame {
                t: k as f64 / s.fps * time_scale,
                width: s.width,
                height: s.height,
                px: cam.frame(k),
            })
            .collect();
        Recorded { frames }
    }
}

impl Source for Recorded {
    fn len(&self) -> usize {
        self.frames.len()
    }
    fn play(&self) -> std::io::Result<Box<dyn Iterator<Item = std::io::Result<Frame>> + '_>> {
        Ok(Box::new(self.frames.iter().map(|f| {
            Ok(Frame {
                t: f.t,
                width: f.width,
                height: f.height,
                px: f.px.clone(),
            })
        })))
    }
}

fn truth(cam: &Camera) -> Summary {
    Summary::of(&cam.truth().iter().map(|t| t.1 * 1000.0).collect::<Vec<_>>())
}

/// Checks the analysis against the truth: median and 95th percentile within one camera frame.
fn check(name: &str, cam: &Camera, opt: &Options, time_scale: f64) -> luch_latency::Analysis {
    let frame_ms = 1000.0 / cam.scene().fps;
    let a = analyze(&Recorded::new(cam, time_scale), opt).unwrap();
    let t = truth(cam);
    let l = &a.latency;
    println!(
        "{name}: measured p50 {:.2} p95 {:.2} min {:.2} max {:.2} jitter {:.2} n {} | true p50 {:.2} p95 {:.2} min {:.2} max {:.2} jitter {:.2} n {} | {:?}",
        l.p50, l.p95, l.min, l.max, l.jitter, l.count, t.p50, t.p95, t.min, t.max, t.jitter, t.count, a.warnings
    );
    assert!(
        l.count as f64 >= 0.6 * t.count as f64,
        "{name}: only {} of {} frames measured",
        l.count,
        t.count
    );
    assert!(
        (l.p50 - t.p50).abs() < frame_ms,
        "{name}: p50 {:.2} vs true {:.2}",
        l.p50,
        t.p50
    );
    assert!(
        (l.p95 - t.p95).abs() < frame_ms,
        "{name}: p95 {:.2} vs true {:.2}",
        l.p95,
        t.p95
    );
    assert!(
        (l.min - t.min).abs() < 1.5 * frame_ms,
        "{name}: min {:.2} vs true {:.2}",
        l.min,
        t.min
    );
    assert!(
        (l.max - t.max).abs() < 1.5 * frame_ms,
        "{name}: max {:.2} vs true {:.2}",
        l.max,
        t.max
    );
    a
}

#[test]
fn steady_stream_is_measured_within_a_camera_frame() {
    let cam = Camera::new(Scene {
        latency: 0.050,
        ..Scene::default()
    });
    let a = check("steady", &cam, &Options::default(), 1.0);
    // With nothing in the way, the median is within a millisecond: a frame caught mid-change places the change
    // within the frame.
    assert!(
        (a.latency.p50 - truth(&cam).p50).abs() < 1.0,
        "p50 {:.2}",
        a.latency.p50
    );
    assert!(a.warnings.is_empty(), "{:?}", a.warnings);
    assert!(
        (a.sender_hz - 60.0).abs() < 0.3,
        "sender {:.2} Hz",
        a.sender_hz
    );
    assert!((a.tv_hz.unwrap() - 60.0).abs() < 0.3, "tv {:?} Hz", a.tv_hz);
    assert!(a.tv_overlay_skipped.unwrap() < 0.02);
    assert!(a.tv_frames_shown > 0.97);
    assert_eq!(a.receiver.unwrap().get("mo"), Some("Synthetic TV"));
    assert_eq!(a.sender.unwrap().get("m"), Some("fast"));
}

#[test]
fn fast_mode_latency_through_a_hard_camera_and_a_tearing_50_hz_tv() {
    let cam = Camera::new(Scene {
        latency: 0.012,
        jitter: 0.004,
        drop: 0.1,
        vsync: false,
        tv_hz: 50.0,
        tv_phase: 0.0031,
        response: 0.004,
        readout: 0.0035,
        blur: 1.3,
        noise: 7.0,
        exposure: 0.9,
        // Stronger perspective, the camera off to one side and a little tilted.
        laptop: [
            pt(40.0, 200.0),
            pt(600.0, 140.0),
            pt(590.0, 500.0),
            pt(60.0, 470.0),
        ],
        tv: [
            pt(650.0, 130.0),
            pt(1250.0, 190.0),
            pt(1240.0, 520.0),
            pt(640.0, 480.0),
        ],
        seed: 7,
        ..Scene::default()
    });
    let a = check("hard", &cam, &Options::default(), 1.0);
    assert!((a.tv_hz.unwrap() - 50.0).abs() < 0.3, "tv {:?} Hz", a.tv_hz);
    // A 50 Hz TV shows at most 50 of the 60 frames a second, and a tenth never reach it.
    assert!(a.tv_frames_shown < 0.85, "shown {:.2}", a.tv_frames_shown);
}

#[test]
fn rolling_shutter_with_the_screens_at_different_heights_is_corrected() {
    let scene = Scene {
        latency: 0.030,
        readout: 0.004,
        laptop: [
            pt(20.0, 20.0),
            pt(600.0, 30.0),
            pt(596.0, 356.0),
            pt(24.0, 346.0),
        ],
        tv: [
            pt(660.0, 370.0),
            pt(1270.0, 360.0),
            pt(1262.0, 705.0),
            pt(655.0, 712.0),
        ],
        seed: 3,
        ..Scene::default()
    };
    let cam = Camera::new(scene);
    let a = check(
        "rolling",
        &cam,
        &Options {
            readout_ms: 4.0,
            ..Options::default()
        },
        1.0,
    );
    assert!(a.warnings.is_empty(), "{:?}", a.warnings);
}

#[test]
fn a_slowed_down_copy_is_put_back_in_real_time() {
    let cam = Camera::new(Scene {
        latency: 0.040,
        seconds: 1.0,
        seed: 5,
        ..Scene::default()
    });
    // Shot at 240 fps, the file plays at 30: every timestamp eight times too far apart.
    let a = check("slowed", &cam, &Options::default(), 8.0);
    assert!(
        (a.time_scale - 8.0).abs() < 0.1,
        "scale {:.2}",
        a.time_scale
    );
    assert!(!a.warnings.is_empty());
}

#[test]
fn without_the_tvs_copy_there_is_no_number_but_a_reason() {
    let mut scene = Scene {
        seconds: 0.5,
        ..Scene::default()
    };
    // The TV out of the picture.
    scene.tv = [
        pt(2000.0, 0.0),
        pt(2600.0, 0.0),
        pt(2600.0, 340.0),
        pt(2000.0, 340.0),
    ];
    let cam = Camera::new(scene);
    let a = analyze(&Recorded::new(&cam, 1.0), &Options::default()).unwrap();
    assert_eq!(a.latency.count, 0);
    assert!(a.warnings[0].contains("only once"), "{:?}", a.warnings);
}

#[test]
fn through_a_real_video_file() {
    if std::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_err()
    {
        eprintln!("ffmpeg is not installed: skipped");
        return;
    }
    let dir = std::env::temp_dir().join(format!("luch-latency-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("synthetic.mp4");
    let cam = Camera::new(Scene {
        latency: 0.025,
        seconds: 1.0,
        seed: 11,
        ..Scene::default()
    });
    luch_latency::cli::encode(&cam, &path).unwrap();
    let video = luch_latency::video::Video::open(&path, None).unwrap();
    assert_eq!(video.len(), cam.frame_count());
    let a = analyze(&video, &Options::default()).unwrap();
    let t = truth(&cam);
    println!(
        "file: measured p50 {:.2} p95 {:.2} | true p50 {:.2} p95 {:.2}",
        a.latency.p50, a.latency.p95, t.p50, t.p95
    );
    assert!((a.latency.p50 - t.p50).abs() < 1000.0 / 240.0);
    let md = luch_latency::report::markdown(&a);
    assert!(
        md.contains("Synthetic TV") && md.contains("ms** median"),
        "{md}"
    );
    let json = luch_latency::report::json(&a);
    assert_eq!(json["receiver"]["mo"], "Synthetic TV");
    assert!(json["samples"].as_array().unwrap().len() > 30);
    std::fs::remove_dir_all(&dir).unwrap();
}
