//! Panopto show/download end to end against loopback KLMS, Panopto and CDN
//! fixtures: launch, delivery metadata, media copy, publish.
use super::*;
use crate::{
    cli::VideosCommand,
    client::{KlmsClient, panopto::Origins},
    fixture_server::{Recorded, Response, Server},
};

const UUID: &str = "11111111-2222-3333-4444-555555555555";
const MP4: &[u8] = b"\x00\x00\x00\x18ftypisomsynthetic-panopto-media";

struct Fixtures {
    klms: Server,
    panopto: Server,
    cdn: Server,
}

fn fixtures(media: fn() -> Response) -> Fixtures {
    let cdn = Server::new(move |request| match request.target.as_str() {
        "/podcast.mp4?sig=synthetic-signature" => media(),
        _ => panic!("unexpected CDN request: {}", request.target),
    });
    let delivery = format!(
        r#"{{"DownloadUrl":null,"Delivery":{{"SessionName":"Synthetic lecture","Duration":12.5,"PodcastStreams":[{{"StreamHttpUrl":"{0}/podcast.mp4?sig=synthetic-signature"}}],"Streams":[{{"StreamUrl":"{0}/camera.m3u8"}}]}}}}"#,
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
        r#"<div id="region-main"><h2>Lecture</h2><iframe src="{}/Panopto/Pages/Embed.aspx?id={UUID}&amp;access_token=synthetic-bearer"></iframe></div>"#,
        panopto.url()
    );
    let klms = Server::new(move |request| match request.target.as_str() {
        "/mod/lti/view.php?id=7" => Response::html(page.clone()),
        _ => panic!("unexpected KLMS request: {}", request.target),
    });
    Fixtures { klms, panopto, cdn }
}

fn run(f: &Fixtures, command: &VideosCommand) -> Result<CommandResult, AppError> {
    let base = Url::parse(&f.klms.url()).unwrap();
    let client = KlmsClient::new(base.as_str(), Some("MoodleSession=synthetic-klms"), 5)
        .unwrap()
        .with_panopto_origins(Origins::fixture(&f.panopto.url(), &f.cdn.url()));
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
    let f = fixtures(mp4);
    let result = run(
        &f,
        &VideosCommand::Show {
            target: "lti:7".into(),
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
fn panopto_download_copies_the_single_mp4_into_a_private_new_file() {
    let f = fixtures(mp4);
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("lecture.mp4");
    let result = run(
        &f,
        &VideosCommand::Download {
            target: format!("panopto-recording:lti:7:{UUID}"),
            out: out.clone(),
            max_bytes: 1024,
            max_seconds: 30,
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
    let f = fixtures(|| Response::bytes("video/mp4", "<!doctype html><html>expired</html>"));
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("lecture.mp4");
    let error = run(
        &f,
        &VideosCommand::Download {
            target: format!("panopto-recording:lti:7:{UUID}"),
            out: out.clone(),
            max_bytes: 1024,
            max_seconds: 30,
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
