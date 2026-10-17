//! Panopto show/download end to end against loopback KLMS, Panopto and CDN
//! fixtures: launch, delivery metadata, media copy, publish.
use super::*;
use crate::{
    cli::VideosCommand,
    client::{KlmsClient, panopto::Config},
    fixture_server::{Recorded, Response, Server},
};
use std::path::Path;

const UUID: &str = "11111111-2222-3333-4444-555555555555";
const MP4: &[u8] = b"\x00\x00\x00\x18ftypisomsynthetic-panopto-media";

struct Fixtures {
    klms: Server,
    panopto: Server,
    cdn: Server,
}

/// A recording whose delivery offers a composed podcast MP4, or (without
/// `podcast`) only an HLS camera stream that needs ffmpeg.
fn fixtures(media: impl Fn() -> Response + Send + Sync + 'static, podcast: bool) -> Fixtures {
    let cdn = Server::new(move |request| match request.target.as_str() {
        "/podcast.mp4?sig=synthetic-signature" => media(),
        _ => panic!("unexpected CDN request: {}", request.target),
    });
    let podcast = if podcast {
        format!(
            r#"[{{"StreamHttpUrl":"{}/podcast.mp4?sig=synthetic-signature"}}]"#,
            cdn.url()
        )
    } else {
        "[]".into()
    };
    let delivery = format!(
        r#"{{"DownloadUrl":null,"Delivery":{{"SessionName":"Synthetic lecture","Duration":12.5,"PodcastStreams":{podcast},"Streams":[{{"StreamUrl":"{}/camera.m3u8"}}]}}}}"#,
        cdn.url()
    );
    let panopto =
        Server::new(
            move |request| match (request.method.as_str(), request.target.as_str()) {
                ("GET", target)
                    if target
                        == format!(
                            "/Panopto/Pages/Embed.aspx?id={UUID}&access_token=synthetic-bearer"
                        ) =>
                {
                    Response::html("<html>player</html>")
                        .header("Set-Cookie", "PanoptoAuth=synthetic-panopto; Path=/")
                        .header("Set-Cookie", "csrfToken=synthetic-csrf; Path=/")
                }
                ("POST", "/Panopto/Pages/Viewer/DeliveryInfo.aspx") => {
                    Response::bytes("application/json", delivery.clone())
                }
                _ => panic!("unexpected Panopto request: {}", request.target),
            },
        );
    let page = format!(
        r#"<body id="page-mod-lti-view" class="path-mod-lti cmid-7"><div id="region-main"><h2>Lecture</h2><iframe src="{}/Panopto/Pages/Embed.aspx?id={UUID}&amp;access_token=synthetic-bearer"></iframe></div>"#,
        panopto.url()
    );
    let klms = Server::new(move |request| match request.target.as_str() {
        "/mod/lti/view.php?id=7" => Response::html(page.clone()),
        _ => panic!("unexpected KLMS request: {}", request.target),
    });
    Fixtures { klms, panopto, cdn }
}

fn run(f: &Fixtures, command: &VideosCommand) -> Result<CommandResult, AppError> {
    run_with(f, command, Config::fixture(&f.panopto.url(), &f.cdn.url()))
}

fn run_with(
    f: &Fixtures,
    command: &VideosCommand,
    config: Config,
) -> Result<CommandResult, AppError> {
    let base = Url::parse(&f.klms.url()).unwrap();
    let client = KlmsClient::new(base.as_str(), Some("MoodleSession=synthetic-klms"), 5)
        .unwrap()
        .with_panopto_config(config);
    let ctx = Ctx {
        client: &client,
        base: &base,
        timeout: 5,
    };
    ctx.videos(command)
}

fn headers(r: &Recorded) -> [Option<&str>; 3] {
    [
        r.header_value("cookie"),
        r.header_value("authorization"),
        r.header_value("x-csrf-token"),
    ]
}

/// Credentials reach only their own origin: the KLMS cookie only KLMS, the
/// Panopto cookie/bearer/CSRF only Panopto metadata, nothing to the CDN.
fn assert_credential_routing(f: &Fixtures, cdn_requests: usize) {
    let klms = f.klms.recorded();
    assert_eq!(klms.len(), 1);
    assert_eq!(klms[0].line, "GET /mod/lti/view.php?id=7 HTTP/1.1");
    assert_eq!(
        headers(&klms[0]),
        [Some("MoodleSession=synthetic-klms"), None, None]
    );
    let panopto = f.panopto.recorded();
    assert_eq!(
        panopto.iter().map(|r| r.line.as_str()).collect::<Vec<_>>(),
        [
            format!(
                "GET /Panopto/Pages/Embed.aspx?id={UUID}&access_token=synthetic-bearer HTTP/1.1"
            )
            .as_str(),
            "POST /Panopto/Pages/Viewer/DeliveryInfo.aspx HTTP/1.1",
        ]
    );
    assert_eq!(headers(&panopto[0]), [None, None, None]);
    assert_eq!(
        headers(&panopto[1]),
        [
            Some("PanoptoAuth=synthetic-panopto; csrfToken=synthetic-csrf"),
            Some("Bearer synthetic-bearer"),
            Some("synthetic-csrf"),
        ]
    );
    assert!(panopto[1].body.contains(&format!("deliveryId={UUID}")));
    let cdn = f.cdn.recorded();
    assert_eq!(cdn.len(), cdn_requests);
    for r in cdn {
        assert_eq!(r.line, "GET /podcast.mp4?sig=synthetic-signature HTTP/1.1");
        assert_eq!(headers(&r), [None, None, None]);
    }
}

fn mp4() -> Response {
    Response::bytes("video/mp4", MP4)
}

#[test]
fn panopto_show_launches_and_reports_delivery_without_media_or_secrets() {
    let f = fixtures(mp4, true);
    let result = run(
        &f,
        &VideosCommand::Show {
            target: format!("{}/mod/lti/view.php?id=7", f.klms.url()),
        },
    )
    .unwrap();
    assert_eq!(
        result.data,
        serde_json::json!({
            "ref": format!("panopto-recording:lti:7:{UUID}"),
            "kind": "panopto",
            "title": "Synthetic lecture",
            "video_type": "recording",
            "course_ref": null,
            "duration_seconds": 12.5,
            "downloadable": true,
            "official_download_available": false,
            "stream_count": 1,
            "output_composition": "composed_podcast",
        })
    );
    assert_credential_routing(&f, 0);
}

#[test]
fn isolated_embedded_activity_launches_without_moodle_layout() {
    let panopto = Server::new(|_| Response::html("player"));
    let iframe = format!("{}/Panopto/Pages/Embed.aspx?id={UUID}", panopto.url());
    let klms = Server::new(move |request| match request.target.as_str() {
        "/mod/panoptocourseembed/view.php?id=7" => Response::html("")
            .status("302 Found")
            .header("Location", "/course/view.php?id=42"),
        "/course/view.php?id=42" => Response::html(format!(
            r#"<main><li id="module-8" class="activity modtype_panoptocourseembed"><iframe src="https://unrelated.example/"></iframe></li><li id="module-7" class="activity modtype_panoptocourseembed"><iframe src="{iframe}"></iframe></li></main>"#
        )),
        _ => panic!("unexpected activity request"),
    });
    let base = Url::parse(&klms.url()).unwrap();
    let client = KlmsClient::new(base.as_str(), Some("MoodleSession=synthetic"), 5)
        .unwrap()
        .with_panopto_config(Config::fixture(&panopto.url(), &panopto.url()));
    let ctx = Ctx {
        client: &client,
        base: &base,
        timeout: 5,
    };
    let (session, _, reference) = ctx.panopto_video("panoptocourseembed:7").unwrap();
    assert_eq!(reference, "panoptocourseembed:7");
    assert_eq!(session.recording_id.as_deref(), Some(UUID));
    assert_eq!(klms.recorded().len(), 2);
    assert_eq!(panopto.recorded().len(), 1);
}

#[test]
fn embedded_activity_show_reads_metadata_from_the_isolated_fragment() {
    for name in [
        "<span class='instancename'>Embedded lecture</span>",
        "<span class='activityname'>Embedded lecture</span>",
        "<span class='activity-title'>Embedded lecture</span>",
        "<a href='/mod/panoptocourseembed/view.php?id=7'>Embedded lecture</a>",
    ] {
        let klms = Server::new(move |request| match request.target.as_str() {
            "/mod/panoptocourseembed/view.php?id=7" => Response::html("")
                .status("302 Found")
                .header("Location", "/course/view.php?id=42"),
            "/course/view.php?id=42" => Response::html(format!(
                "<main><div class='activity-item' data-id='7'>{name}</div><li id='module-8'><span class='instancename'>Other module</span></li></main>"
            )),
            _ => panic!("unexpected activity request"),
        });
        let base = Url::parse(&klms.url()).unwrap();
        let client = KlmsClient::new(base.as_str(), Some("MoodleSession=synthetic"), 5).unwrap();
        let ctx = Ctx {
            client: &client,
            base: &base,
            timeout: 5,
        };
        let (page, kind) = ctx.video_page("panoptocourseembed:7").unwrap();
        let video = ctx
            .activity_video(&page, kind, "panoptocourseembed:7")
            .unwrap();
        assert_eq!(video.reference, "panoptocourseembed:7");
        assert_eq!(video.title, "Embedded lecture");
    }
}

#[test]
fn panopto_download_copies_the_single_mp4_into_a_private_new_file() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().to_path_buf();
    let f = fixtures(
        move || {
            assert!(std::fs::read_dir(&parent).unwrap().all(|entry| {
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".scratch")
            }));
            mp4()
        },
        true,
    );
    let out = dir.path().join("lecture.mp4");
    let result = run(
        &f,
        &VideosCommand::Download {
            target: format!("panopto-recording:lti:7:{UUID}"),
            out: out.clone(),
            max_bytes: 1024,
            max_seconds: 30,
            stall_seconds: 10,
        },
    )
    .unwrap();
    assert_eq!(std::fs::read(&out).unwrap(), MP4);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&out).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert_eq!(result.data["bytes"], MP4.len());
    assert_eq!(result.data["content_type"], "video/mp4");
    assert_eq!(
        result.data["source_url"],
        format!("{}/mod/lti/view.php?id=7", f.klms.url())
    );
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        1,
        "no temporary files remain"
    );
    assert_credential_routing(&f, 1);
}

#[test]
fn panopto_download_rejects_html_media_and_publishes_nothing() {
    let f = fixtures(
        || Response::bytes("video/mp4", "<!doctype html><html>expired</html>"),
        true,
    );
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("lecture.mp4");
    let error = run(
        &f,
        &VideosCommand::Download {
            target: format!("panopto-recording:lti:7:{UUID}"),
            out: out.clone(),
            max_bytes: 1024,
            max_seconds: 30,
            stall_seconds: 10,
        },
    )
    .err()
    .unwrap();
    assert_eq!(
        (error.code, error.message.as_str()),
        (
            "CONTENT_UNAVAILABLE",
            "Panopto source returned non-MP4 content"
        )
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    assert_credential_routing(&f, 1);
}

fn download(out: &Path) -> VideosCommand {
    VideosCommand::Download {
        target: format!("panopto-recording:lti:7:{UUID}"),
        out: out.to_path_buf(),
        max_bytes: 1024,
        max_seconds: 30,
        stall_seconds: 10,
    }
}

#[test]
fn missing_ffmpeg_fails_before_any_media_is_fetched() {
    let f = fixtures(mp4, false);
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("no-such-ffmpeg");
    let config = Config::fixture(&f.panopto.url(), &f.cdn.url()).with_ffmpeg(&missing);
    let error = run_with(&f, &download(&dir.path().join("lecture.mp4")), config)
        .err()
        .unwrap();
    assert_eq!(
        (error.code, error.message.as_str()),
        (
            "CONFIG_ERROR",
            "ffmpeg is required for HLS or multiple streams; install ffmpeg and retry"
        )
    );
    assert_credential_routing(&f, 0);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn download_sweeps_only_stale_temporaries_beside_the_output() {
    use std::time::{Duration, SystemTime};
    let f = fixtures(mp4, true);
    let dir = tempfile::tempdir().unwrap();
    let old = SystemTime::now() - Duration::from_secs(49 * 60 * 60);
    let age = |path: &Path| {
        std::fs::File::open(path)
            .unwrap()
            .set_modified(old)
            .unwrap()
    };
    let stale_part = dir.path().join(".klms-download-4294967294-ab.part");
    let stale_retry = dir.path().join(".klms-download-4294967294-ab.part.1a2");
    let stale_scratch = dir.path().join(".klms-download-4294967294-cd.scratch");
    let reused_pid = dir
        .path()
        .join(format!(".klms-download-{}-ab.part", std::process::id()));
    let fresh_part = dir.path().join(".klms-download-4294967294-ef.part");
    let symlink = dir.path().join(".klms-download-4294967294-ff.part");
    let unrelated = dir.path().join("notes.part");
    let misnamed_dir = dir.path().join(".klms-download-4294967294-12.part");
    for file in [
        &stale_part,
        &stale_retry,
        &reused_pid,
        &fresh_part,
        &unrelated,
    ] {
        std::fs::write(file, b"partial").unwrap();
    }
    std::fs::create_dir(&stale_scratch).unwrap();
    std::fs::write(stale_scratch.join("stream-0.mp4"), b"fragment").unwrap();
    std::fs::create_dir(&misnamed_dir).unwrap();
    for path in [
        &stale_part,
        &stale_retry,
        &reused_pid,
        &unrelated,
        &misnamed_dir,
    ] {
        age(path);
    }
    std::os::unix::fs::symlink(&unrelated, &symlink).unwrap();
    age(&stale_scratch);
    // The maximum 24-hour download deadline must remain within the retention window.
    std::fs::File::open(&fresh_part)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(24 * 60 * 60))
        .unwrap();
    run(&f, &download(&dir.path().join("lecture.mp4"))).unwrap();
    let mut left = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    left.sort();
    assert_eq!(
        left,
        [
            ".klms-download-4294967294-12.part",
            ".klms-download-4294967294-ef.part",
            ".klms-download-4294967294-ff.part",
            "lecture.mp4",
            "notes.part"
        ]
    );
}

#[test]
fn collection_human_output_includes_recording_refs_titles_and_durations() {
    let f = collection_fixture(false);
    let result = run(
        &f,
        &VideosCommand::Show {
            target: "lti:7".into(),
        },
    )
    .unwrap();
    assert!(
        result
            .human
            .contains(&format!("panopto-recording:lti:7:{UUID}"))
    );
    assert!(result.human.contains("Lecture one"));
    assert!(result.human.contains("12.5"));
    assert!(result.human.contains("DURATION (SECONDS)"));
    assert_eq!(result.data["complete"], true);
}

fn collection_fixture(fail_second_page: bool) -> Fixtures {
    let cdn = Server::new(|_| panic!("metadata show must not fetch media"));
    let page = std::sync::atomic::AtomicUsize::new(0);
    let panopto = Server::new(move |r| {
        if r.method == "GET" {
            return Response::html("player");
        }
        if page.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            Response::bytes(
                "application/json",
                serde_json::json!({"d":{"Results":[
                {"DeliveryID":UUID,"SessionName":"Lecture one","Duration":12.5}
            ],"TotalNumber":1}})
                .to_string(),
            )
        } else if fail_second_page {
            Response::html("failed enumeration").status("500 Internal Server Error")
        } else {
            Response::bytes(
                "application/json",
                serde_json::json!({"d":{"Results":[],"TotalNumber":1}}).to_string(),
            )
        }
    });
    let html = format!(
        r#"<body id="page-mod-lti-view" class="path-mod-lti cmid-7"><main><h1>Lecture collection</h1><iframe src="{}/Panopto/Pages/Sessions/List.aspx?folderID={UUID}"></iframe></main>"#,
        panopto.url()
    );
    let klms = Server::new(move |_| Response::html(html.clone()));
    Fixtures { klms, panopto, cdn }
}

#[test]
fn failed_folder_page_remains_an_error() {
    let f = collection_fixture(true);
    assert!(
        run(
            &f,
            &VideosCommand::Show {
                target: "lti:7".into()
            }
        )
        .is_err()
    );
}

#[test]
fn show_keeps_activity_metadata_with_a_warning_when_launch_or_viewer_fails() {
    for (target, native) in [("lti:7", false), ("vod:7", true)] {
        let panopto = Server::new(|_| panic!("unsupported launch must not be contacted"));
        let cdn = Server::new(|_| panic!("metadata must not fetch media"));
        let klms = Server::new(move |request| {
            if native && request.target == "/mod/vod/viewer/index.php?id=7" {
                Response::html("<main>unsupported player</main>")
            } else {
                let kind = if native { "vod" } else { "lti" };
                Response::html(format!(
                    "<body id='page-mod-{kind}-view' class='path-mod-{kind} cmid-7'><main><h1>Fixture recording</h1><div id='intro'>Activity description</div></main></body>",
                ))
            }
        });
        let f = Fixtures { klms, panopto, cdn };
        let result = run(
            &f,
            &VideosCommand::Show {
                target: target.into(),
            },
        )
        .unwrap();
        assert_eq!(result.data["ref"], target);
        assert_eq!(result.data["title"], "Fixture recording");
        assert!(result.data["downloadable"].is_null());
        assert!(result.human.contains("Fixture recording"));
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.contains("playback inspection failed"))
        );
        assert_eq!(f.klms.requests().len(), if native { 2 } else { 1 });
    }
}

#[test]
fn show_keeps_activity_metadata_when_delivery_inspection_fails() {
    let cdn = Server::new(|_| panic!("metadata must not fetch media"));
    let panopto = Server::new(|r| {
        if r.method == "GET" {
            Response::html("player")
        } else {
            Response::bytes("application/json", "{}")
        }
    });
    let page = format!(
        r#"<body id="page-mod-lti-view" class="path-mod-lti cmid-7"><main><h1>Fixture recording</h1><iframe src="{}/Panopto/Pages/Embed.aspx?id={UUID}"></iframe></main>"#,
        panopto.url()
    );
    let klms = Server::new(move |_| Response::html(page.clone()));
    let f = Fixtures { klms, panopto, cdn };
    let result = run(
        &f,
        &VideosCommand::Show {
            target: "lti:7".into(),
        },
    )
    .unwrap();
    assert_eq!(result.data["ref"], "lti:7");
    assert_eq!(result.data["title"], "Fixture recording");
    assert!(result.data["downloadable"].is_null());
    assert_eq!(result.warnings.len(), 1);
    assert_eq!(f.klms.requests().len(), 1);
}
