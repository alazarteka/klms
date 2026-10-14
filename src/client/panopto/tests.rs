use super::*;
use serde_json::json;
const ID: &str = "00000000-0000-0000-0000-000000000001";
fn limits(seconds: u64) -> Limits {
    Limits::new(10_000_000, seconds, seconds)
}
fn prod() -> Config {
    Config::production().unwrap()
}
fn cdn(path: &str) -> String {
    format!("https://s-cloudfront.cdn.ap.panopto.com/{path}")
}
#[test]
fn readable_sources_are_independent_of_official_download_permission() {
    let v = json!({"DownloadUrl":null,"Delivery":{"SessionName":"synthetic","Duration":2.0,"PodcastStreams":[{"StreamHttpUrl":cdn("podcast.mp4")}],"Streams":[{"StreamUrl":cdn("video.m3u8")},{"StreamUrl":cdn("audio.m3u8")}]}});
    let d = parse_delivery(ID, &v, &prod()).unwrap();
    assert!(!d.official_download_available);
    assert!(d.mp4_available);
    assert_eq!(d.groups.len(), 1);
    assert_eq!(d.groups[0].mp4.as_ref().unwrap().path(), "/podcast.mp4");
    let serialized = serde_json::to_string(&d).unwrap();
    assert!(!serialized.contains("cloudfront"));
}
#[test]
fn mixed_and_missing_streams_cannot_silently_download_partial_recordings() {
    let v = json!({"Delivery":{"Streams":[{"StreamUrl":cdn("camera.mp4")},{"StreamUrl":cdn("audio.m3u8"),"RelativeStart":1.5}]}});
    let d = parse_delivery(ID, &v, &prod()).unwrap();
    assert!(!d.mp4_available);
    assert!(d.hls_available);
    assert_eq!(d.groups.len(), 2);
    assert_eq!(d.groups[1].offset, 1.5);
    let v = json!({"Delivery":{"Streams":[{"StreamUrl":cdn("camera.mp4")},{"StreamUrl":"https://unrelated.test/audio.mp4"}]}});
    let d = parse_delivery(ID, &v, &prod()).unwrap();
    assert!(!d.mp4_available && !d.hls_available);
    assert!(d.groups[1].mp4.is_none());
}
fn fixture_fetch<'a>(
    items: &'a [(&'a str, &'a [u8])],
    seen: &'a mut Vec<String>,
) -> impl FnMut(&Url, &mut dyn Write, Limits, Option<ByteRange>) -> Result<Download, AppError> + 'a
{
    move |u, w, limits, range| {
        let max = limits.max_bytes;
        assert!(prod().is_cdn(u));
        seen.push(u.path().into());
        let data = items.iter().find(|(path, _)| *path == u.path()).unwrap().1;
        let data = range.map_or(data, |(start, len)| {
            &data[start as usize..(start + len) as usize]
        });
        if data.len() > max {
            return Err(AppError::limit("fixture exceeds bounded retrieval"));
        }
        w.write_all(data).unwrap();
        Ok(Download {
            bytes: data.len(),
            content_type: "application/octet-stream",
        })
    }
}
#[test]
fn hls_fetches_all_fragments_and_writes_only_secret_free_local_paths() {
    let scratch = tempfile::tempdir().unwrap();
    let mut seen = Vec::new();
    let items=[("/master.m3u8",b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\nlow.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=2\nhigh.m3u8?token=secret\n".as_slice()),("/high.m3u8",b"#EXTM3U\n#EXT-X-MAP:URI=\"init.mp4?token=secret\"\n#EXTINF:1,\na.ts?token=secret\n#EXTINF:1,\nb.ts?token=secret\n#EXT-X-ENDLIST\n"), ("/init.mp4",b"init"),("/a.ts",b"segment1"),("/b.ts",b"segment2")];
    let mut fetch = fixture_fetch(&items, &mut seen);
    let mut budget = 10000;
    let mut index = 0;
    let path = local_playlist(
        &Url::parse(&cdn("master.m3u8")).unwrap(),
        scratch.path(),
        &mut index,
        &mut budget,
        limits(5),
        0,
        &mut fetch,
    )
    .unwrap();
    drop(fetch);
    assert_eq!(
        seen,
        vec!["/master.m3u8", "/high.m3u8", "/init.mp4", "/a.ts", "/b.ts"]
    );
    let text = std::fs::read_to_string(path).unwrap();
    assert!(!text.contains("token") && !text.contains("secret") && !text.contains("https:"));
    assert!(text.contains("#EXT-X-ENDLIST"));
    for file in std::fs::read_dir(scratch.path()).unwrap() {
        let bytes = std::fs::read(file.unwrap().path()).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("secret"));
    }
}
#[test]
fn hls_playlist_size_encryption_and_live_fail_before_segment_fetch() {
    for playlist in [
        "#EXTM3U\n#EXTINF:1,\na.ts\n",
        "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"secret.key\"\n#EXTINF:1,\na.ts\n#EXT-X-ENDLIST\n",
    ] {
        let scratch = tempfile::tempdir().unwrap();
        let mut seen = Vec::new();
        let items = [("/test.m3u8", playlist.as_bytes())];
        let mut fetch = fixture_fetch(&items, &mut seen);
        let e = local_playlist(
            &Url::parse(&cdn("test.m3u8")).unwrap(),
            scratch.path(),
            &mut 0,
            &mut 2000,
            limits(5),
            0,
            &mut fetch,
        )
        .unwrap_err();
        assert_eq!(e.code, "CONTENT_UNAVAILABLE");
        drop(fetch);
        assert_eq!(seen.len(), 1);
    }
    let scratch = tempfile::tempdir().unwrap();
    let mut cap_seen = 0;
    let mut fetch = |_: &Url, _: &mut dyn Write, limits: Limits, _: Option<ByteRange>| {
        cap_seen = limits.max_bytes;
        Err(AppError::limit("oversized playlist"))
    };
    let error = local_playlist(
        &Url::parse(&cdn("large.m3u8")).unwrap(),
        scratch.path(),
        &mut 0,
        &mut (4 * 1024 * 1024),
        limits(5),
        0,
        &mut fetch,
    )
    .err()
    .unwrap();
    assert_eq!(error.code, "LIMIT_EXCEEDED");
    assert_eq!(cap_seen, 1024 * 1024);
}
#[test]
fn hls_master_retrieves_default_separate_audio_without_exposing_remote_uri() {
    let scratch = tempfile::tempdir().unwrap();
    let mut seen = Vec::new();
    let items=[("/master.m3u8",b"#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"audio\",NAME=\"a\",DEFAULT=YES,URI=\"audio.m3u8?secret=x\"\n#EXT-X-STREAM-INF:BANDWIDTH=1,AUDIO=\"audio\"\nvideo.m3u8\n".as_slice()),("/video.m3u8",b"#EXTM3U\n#EXTINF:1,\nv.ts\n#EXT-X-ENDLIST\n"),("/audio.m3u8",b"#EXTM3U\n#EXTINF:1,\na.ts\n#EXT-X-ENDLIST\n"),("/v.ts",b"video"),("/a.ts",b"audio")];
    let mut fetch = fixture_fetch(&items, &mut seen);
    let p = local_playlist(
        &Url::parse(&cdn("master.m3u8")).unwrap(),
        scratch.path(),
        &mut 0,
        &mut 10000,
        limits(5),
        0,
        &mut fetch,
    )
    .unwrap();
    drop(fetch);
    let content = std::fs::read_to_string(p).unwrap();
    assert!(content.contains("TYPE=AUDIO"));
    assert!(!content.contains("secret"));
    assert!(seen.contains(&"/a.ts".into()) && seen.contains(&"/v.ts".into()));
}

#[test]
fn local_hls_remux_preserves_video_audio_or_reports_missing_ffmpeg() {
    use std::process::{Command, Stdio};
    let scratch = tempfile::tempdir().unwrap();
    let available = Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if available
        .as_ref()
        .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
    {
        let e = remux(
            "ffmpeg".as_ref(),
            &[scratch.path().join("input.m3u8")],
            &[0.0],
            scratch.path(),
            &mut Vec::new(),
            limits(5),
        )
        .err()
        .unwrap();
        assert_eq!(e.code, "CONFIG_ERROR");
        assert!(e.message.contains("install ffmpeg"));
        return;
    }
    assert!(available.unwrap().success());
    let remote = tempfile::tempdir().unwrap();
    let playlist = remote.path().join("source.m3u8");
    let status = Command::new("ffmpeg")
        .args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=blue:s=64x64:r=4",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440",
            "-t",
            "2",
            "-c:v",
            "mpeg2video",
            "-c:a",
            "aac",
            "-f",
            "hls",
            "-hls_time",
            "1",
            "-hls_list_size",
            "0",
        ])
        .arg(&playlist)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    let mut fetch = |u: &Url, w: &mut dyn Write, limits: Limits, _: Option<ByteRange>| {
        let max = limits.max_bytes;
        assert!(prod().is_cdn(u));
        let p = remote.path().join(u.path().trim_start_matches('/'));
        let data = std::fs::read(p).unwrap();
        if data.len() > max {
            return Err(AppError::limit("fixture too large"));
        }
        w.write_all(&data).unwrap();
        Ok(Download {
            bytes: data.len(),
            content_type: "application/octet-stream",
        })
    };
    let mut budget = 1000000;
    let p = local_playlist(
        &Url::parse(&cdn("source.m3u8")).unwrap(),
        scratch.path(),
        &mut 0,
        &mut budget,
        limits(5),
        0,
        &mut fetch,
    )
    .unwrap();
    let mut output = Vec::new();
    let d = remux(
        "ffmpeg".as_ref(),
        &[p],
        &[0.0],
        scratch.path(),
        &mut output,
        limits(10),
    )
    .unwrap();
    assert_eq!(d.bytes, output.len());
    assert_eq!(&output[4..8], b"ftyp");
    let saved = remote.path().join("verified.mp4");
    std::fs::write(&saved, &output).unwrap();
    let decoded = Command::new("ffmpeg")
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-i"])
        .arg(&saved)
        .args(["-map", "0:v:0", "-map", "0:a:0", "-f", "null", "-"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(decoded.success());
}

#[test]
fn hls_byte_ranges_retrieve_exact_init_and_segments_and_remove_range_tags() {
    let scratch = tempfile::tempdir().unwrap();
    let mut seen = Vec::new();
    let items=[("/test.m3u8",b"#EXTM3U\n#EXT-X-MAP:URI=\"shared.mp4?secret=token\",BYTERANGE=\"4@0\"\n#EXTINF:1,\n#EXT-X-BYTERANGE:3@4\nshared.mp4?secret=token\n#EXTINF:1,\n#EXT-X-BYTERANGE:3\nshared.mp4?secret=token\n#EXT-X-ENDLIST\n".as_slice()),("/shared.mp4",b"INITaaabbbNOTRETRIEVED")];
    let mut fetch = fixture_fetch(&items, &mut seen);
    let mut budget = 10000;
    let p = local_playlist(
        &Url::parse(&cdn("test.m3u8")).unwrap(),
        scratch.path(),
        &mut 0,
        &mut budget,
        limits(5),
        0,
        &mut fetch,
    )
    .unwrap();
    drop(fetch);
    let local = std::fs::read_to_string(p).unwrap();
    assert!(!local.contains("BYTERANGE") && !local.contains("secret"));
    let mut fragments = std::fs::read_dir(scratch.path())
        .unwrap()
        .map(|f| f.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("fragment")
        })
        .map(|p| std::fs::read(p).unwrap())
        .collect::<Vec<_>>();
    fragments.sort();
    assert_eq!(
        fragments,
        vec![b"INIT".to_vec(), b"aaa".to_vec(), b"bbb".to_vec()]
    );
}

#[test]
fn composed_mp4_takes_precedence_over_an_alternative_hls_podcast() {
    let value = json!({"Delivery":{"PodcastStreams":[{"StreamUrl":cdn("podcast.m3u8")},{"StreamHttpUrl":cdn("podcast.mp4")}],"Streams":[{"StreamUrl":cdn("camera.m3u8")},{"StreamUrl":cdn("slides.m3u8")}]}});
    let delivery = parse_delivery(ID, &value, &prod()).unwrap();
    assert!(delivery.mp4_available);
    assert_eq!(delivery.composition, "composed_podcast");
    assert_eq!(delivery.output_stream_count, 1);
    assert_eq!(
        delivery.groups[0].mp4.as_ref().unwrap().path(),
        "/podcast.mp4"
    );
}

fn cdn_fixture(
    response: fn() -> crate::fixture_server::Response,
) -> (crate::fixture_server::Server, Config) {
    let server = crate::fixture_server::Server::new(move |_| response());
    let config = Config::fixture("https://kaist.ap.panopto.com/", &server.url());
    (server, config)
}

#[test]
fn media_range_must_match_the_requested_bytes() {
    use crate::fixture_server::Response;
    let (server, config) = cdn_fixture(|| {
        Response::bytes("video/mp2t", "bbbb")
            .status("206 Partial Content")
            .header("Content-Range", "bytes 1-4/10")
    });
    let source = Url::parse(&format!("{}/shared.ts", server.url())).unwrap();
    let mut out = Vec::new();
    let error = media_copy(
        &config,
        &source,
        &mut out,
        limits(5),
        Expect::Media,
        Some((0, 4)),
    )
    .err()
    .unwrap();
    assert_eq!(
        (error.code, error.message.as_str()),
        (
            "CONTENT_UNAVAILABLE",
            "HLS server returned an unexpected byte range"
        )
    );
    assert!(out.is_empty());
    assert_eq!(
        server.recorded()[0].header_value("range"),
        Some("bytes=0-3")
    );
}

#[test]
fn media_that_sniffs_as_html_is_refused_before_writing() {
    use crate::fixture_server::Response;
    let (server, config) = cdn_fixture(|| {
        Response::bytes(
            "application/octet-stream",
            "<!DOCTYPE html><html>denied</html>",
        )
    });
    let source = Url::parse(&format!("{}/segment.ts", server.url())).unwrap();
    let mut out = Vec::new();
    let error = media_copy(&config, &source, &mut out, limits(5), Expect::Media, None)
        .err()
        .unwrap();
    assert_eq!(
        (error.code, error.message.as_str()),
        (
            "CONTENT_UNAVAILABLE",
            "Panopto media returned HTML instead of video"
        )
    );
    assert!(out.is_empty());
}

#[cfg(unix)]
fn fake_ffmpeg(dir: &Path, script: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let exe = dir.join("ffmpeg");
    std::fs::write(&exe, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
    exe
}

#[cfg(unix)]
#[test]
fn ffmpeg_version_has_a_short_deadline_and_is_reaped() {
    let dir = tempfile::tempdir().unwrap();
    let pid = dir.path().join("pid");
    let exe = fake_ffmpeg(
        dir.path(),
        &format!("echo $$ > '{}'\nexec sleep 30", pid.display()),
    );
    for limits in [
        Limits {
            deadline: Instant::now() + Duration::from_millis(100),
            ..limits(10)
        },
        limits(10),
    ] {
        let start = Instant::now();
        let error = require_ffmpeg(exe.as_os_str(), limits).unwrap_err();
        assert_eq!(error.code, "NETWORK_ERROR");
        assert!(start.elapsed() < Duration::from_secs(3));
        #[cfg(target_os = "linux")]
        assert!(
            !Path::new("/proc")
                .join(std::fs::read_to_string(&pid).unwrap().trim())
                .exists()
        );
    }
}

#[cfg(unix)]
#[test]
fn remux_timeout_is_network_error_and_reaps_child() {
    let dir = tempfile::tempdir().unwrap();
    let pid = dir.path().join("pid");
    let exe = fake_ffmpeg(
        dir.path(),
        &format!("echo $$ > '{}'\nexec sleep 30", pid.display()),
    );
    let limits = Limits {
        deadline: Instant::now() + Duration::from_millis(100),
        ..limits(10)
    };
    let error = remux(
        exe.as_os_str(),
        &[],
        &[],
        dir.path(),
        &mut Vec::new(),
        limits,
    )
    .err()
    .unwrap();
    assert_eq!(error.code, "NETWORK_ERROR");
    #[cfg(target_os = "linux")]
    assert!(
        !Path::new("/proc")
            .join(std::fs::read_to_string(pid).unwrap().trim())
            .exists()
    );
}

#[cfg(unix)]
#[test]
fn remux_checks_deadline_after_final_write_and_removes_source_on_success() {
    struct SlowWriter;
    impl Write for SlowWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            std::thread::sleep(Duration::from_millis(200));
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let exe = fake_ffmpeg(
        dir.path(),
        "for arg do out=$arg; done\nprintf 'synthetic-mp4' > \"$out\"",
    );
    let short = Limits {
        deadline: Instant::now() + Duration::from_millis(100),
        ..limits(10)
    };
    let error = remux(
        exe.as_os_str(),
        &[],
        &[],
        scratch.path(),
        &mut SlowWriter,
        short,
    )
    .err()
    .unwrap();
    assert_eq!(error.code, "NETWORK_ERROR");
    std::fs::remove_file(scratch.path().join("remux.mp4")).unwrap();
    let mut output = Vec::new();
    remux(
        exe.as_os_str(),
        &[],
        &[],
        scratch.path(),
        &mut output,
        limits(10),
    )
    .unwrap();
    assert_eq!(output, b"synthetic-mp4");
    assert!(!scratch.path().join("remux.mp4").exists());
}

#[cfg(unix)]
#[test]
fn remux_byte_cap_is_limit_error_and_stderr_tail_is_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let exe = fake_ffmpeg(
        dir.path(),
        "for arg do out=$arg; done\nprintf 'synthetic-mp4' > \"$out\"",
    );
    let error = remux(
        exe.as_os_str(),
        &[],
        &[],
        dir.path(),
        &mut Vec::new(),
        limits(10).with_max(3),
    )
    .err()
    .unwrap();
    assert_eq!(error.code, "LIMIT_EXCEEDED");
    let exe = fake_ffmpeg(
        dir.path(),
        "i=0; while [ $i -lt 1000 ]; do printf 'diagnostic-line\\n' >&2; i=$((i+1)); done\nprintf 'LAST DIAGNOSTIC' >&2\nexit 1",
    );
    let error = remux(
        exe.as_os_str(),
        &[],
        &[],
        dir.path(),
        &mut Vec::new(),
        limits(10),
    )
    .err()
    .unwrap();
    assert_eq!(error.code, "CONTENT_UNAVAILABLE");
    assert!(error.message.ends_with("LAST DIAGNOSTIC"));
    assert!(error.message.len() <= STDERR_TAIL + 100);
}

#[test]
fn panopto_media_stall_timeout_bounds_response_headers() {
    use crate::fixture_server::{Response, Server};
    let server = Server::new(|_| {
        std::thread::sleep(Duration::from_secs(2));
        Response::bytes("video/mp4", b"\0\0\0\x18ftypisommedia")
    });
    let config = Config::fixture("https://kaist.ap.panopto.com/", &server.url());
    let source = Url::parse(&format!("{}/a.mp4", server.url())).unwrap();
    let limits = Limits {
        stall: Duration::from_millis(100),
        ..limits(5)
    };
    let start = Instant::now();
    let mut output = Vec::new();
    let error = media_copy(&config, &source, &mut output, limits, Expect::Mp4, None)
        .err()
        .unwrap();
    assert_eq!(error.code, "NETWORK_ERROR");
    assert!(start.elapsed() < Duration::from_secs(1));
    assert!(output.is_empty());
}

#[test]
fn corrupt_middle_hls_segment_fails_instead_of_publishing_a_gap() {
    if Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
    {
        return;
    }
    let remote = tempfile::tempdir().unwrap();
    let playlist = remote.path().join("source.m3u8");
    let status = Command::new("ffmpeg")
        .args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=blue:s=64x64:r=10",
            "-t",
            "3",
            "-c:v",
            "mpeg2video",
            "-g",
            "10",
            "-f",
            "hls",
            "-hls_time",
            "1",
            "-hls_list_size",
            "0",
        ])
        .arg(&playlist)
        .status()
        .unwrap();
    assert!(status.success());
    let raw = std::fs::read_to_string(&playlist).unwrap();
    let segments = raw
        .lines()
        .filter(|line| !line.starts_with('#'))
        .collect::<Vec<_>>();
    assert_eq!(segments.len(), 3);
    std::fs::write(remote.path().join(segments[1]), b"corrupt middle segment").unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let mut fetch = |u: &Url, w: &mut dyn Write, limits: Limits, _: Option<ByteRange>| {
        let data = std::fs::read(remote.path().join(u.path().trim_start_matches('/'))).unwrap();
        assert!(data.len() <= limits.max_bytes);
        w.write_all(&data).unwrap();
        Ok(Download {
            bytes: data.len(),
            content_type: "application/octet-stream",
        })
    };
    let input = local_playlist(
        &Url::parse(&cdn("source.m3u8")).unwrap(),
        scratch.path(),
        &mut 0,
        &mut 1_000_000,
        limits(5),
        0,
        &mut fetch,
    )
    .unwrap();
    let mut output = Vec::new();
    let error = remux(
        "ffmpeg".as_ref(),
        &[input],
        &[0.0],
        scratch.path(),
        &mut output,
        limits(5),
    )
    .err()
    .unwrap();
    assert_eq!(error.code, "CONTENT_UNAVAILABLE");
    assert!(error.message.contains("ffmpeg could not remux"));
    assert!(error.message.len() > "ffmpeg could not remux downloaded video fragments: ".len());
    assert!(output.is_empty());
}
