use super::*;
use serde_json::json;
const ID: &str = "00000000-0000-0000-0000-000000000001";
fn cdn(path: &str) -> String {
    format!("https://s-cloudfront.cdn.ap.panopto.com/{path}")
}
#[test]
fn readable_sources_are_independent_of_official_download_permission() {
    let v = json!({"DownloadUrl":null,"Delivery":{"SessionName":"synthetic","Duration":2.0,"PodcastStreams":[{"StreamHttpUrl":cdn("podcast.mp4")}],"Streams":[{"StreamUrl":cdn("video.m3u8")},{"StreamUrl":cdn("audio.m3u8")}]}});
    let d = parse_delivery(ID, &v).unwrap();
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
    let d = parse_delivery(ID, &v).unwrap();
    assert!(!d.mp4_available);
    assert!(d.hls_available);
    assert_eq!(d.groups.len(), 2);
    assert_eq!(d.groups[1].offset, 1.5);
    let v = json!({"Delivery":{"Streams":[{"StreamUrl":cdn("camera.mp4")},{"StreamUrl":"https://unrelated.test/audio.mp4"}]}});
    let d = parse_delivery(ID, &v).unwrap();
    assert!(!d.mp4_available && !d.hls_available);
    assert!(d.groups[1].mp4.is_none());
}
fn fixture_fetch<'a>(
    items: &'a [(&'a str, &'a [u8])],
    seen: &'a mut Vec<String>,
) -> impl FnMut(&Url, &mut dyn Write, usize, u64, Option<ByteRange>) -> Result<Download, AppError> + 'a
{
    move |u, w, max, _, range| {
        assert!(allowed_media(u));
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
    let scratch = Scratch::new().unwrap();
    let mut seen = Vec::new();
    let items=[("/master.m3u8",b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\nlow.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=2\nhigh.m3u8?token=secret\n".as_slice()),("/high.m3u8",b"#EXTM3U\n#EXT-X-MAP:URI=\"init.mp4?token=secret\"\n#EXTINF:1,\na.ts?token=secret\n#EXTINF:1,\nb.ts?token=secret\n#EXT-X-ENDLIST\n"), ("/init.mp4",b"init"),("/a.ts",b"segment1"),("/b.ts",b"segment2")];
    let mut fetch = fixture_fetch(&items, &mut seen);
    let mut budget = 10000;
    let mut index = 0;
    let path = local_playlist_with(
        &Url::parse(&cdn("master.m3u8")).unwrap(),
        &scratch,
        &mut index,
        &mut budget,
        Instant::now() + Duration::from_secs(5),
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
    for file in std::fs::read_dir(&scratch.0).unwrap() {
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
        let scratch = Scratch::new().unwrap();
        let mut seen = Vec::new();
        let items = [("/test.m3u8", playlist.as_bytes())];
        let mut fetch = fixture_fetch(&items, &mut seen);
        let e = local_playlist_with(
            &Url::parse(&cdn("test.m3u8")).unwrap(),
            &scratch,
            &mut 0,
            &mut 2000,
            Instant::now() + Duration::from_secs(5),
            0,
            &mut fetch,
        )
        .unwrap_err();
        assert_eq!(e.code, "CONTENT_UNAVAILABLE");
        drop(fetch);
        assert_eq!(seen.len(), 1);
    }
    let scratch = Scratch::new().unwrap();
    let mut cap_seen = 0;
    let mut fetch = |_: &Url, _: &mut dyn Write, max: usize, _: u64, _: Option<ByteRange>| {
        cap_seen = max;
        Err(AppError::limit("oversized playlist"))
    };
    assert!(
        local_playlist_with(
            &Url::parse(&cdn("large.m3u8")).unwrap(),
            &scratch,
            &mut 0,
            &mut (4 * 1024 * 1024),
            Instant::now() + Duration::from_secs(5),
            0,
            &mut fetch
        )
        .is_err()
    );
    assert_eq!(cap_seen, 1024 * 1024);
}
#[test]
fn hls_master_retrieves_default_separate_audio_without_exposing_remote_uri() {
    let scratch = Scratch::new().unwrap();
    let mut seen = Vec::new();
    let items=[("/master.m3u8",b"#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"audio\",NAME=\"a\",DEFAULT=YES,URI=\"audio.m3u8?secret=x\"\n#EXT-X-STREAM-INF:BANDWIDTH=1,AUDIO=\"audio\"\nvideo.m3u8\n".as_slice()),("/video.m3u8",b"#EXTM3U\n#EXTINF:1,\nv.ts\n#EXT-X-ENDLIST\n"),("/audio.m3u8",b"#EXTM3U\n#EXTINF:1,\na.ts\n#EXT-X-ENDLIST\n"),("/v.ts",b"video"),("/a.ts",b"audio")];
    let mut fetch = fixture_fetch(&items, &mut seen);
    let p = local_playlist_with(
        &Url::parse(&cdn("master.m3u8")).unwrap(),
        &scratch,
        &mut 0,
        &mut 10000,
        Instant::now() + Duration::from_secs(5),
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
    let scratch = Scratch::new().unwrap();
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
            &[scratch.0.join("input.m3u8")],
            &scratch,
            &mut Vec::new(),
            1000000,
            Instant::now() + Duration::from_secs(5),
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
    let mut fetch = |u: &Url, w: &mut dyn Write, max: usize, _: u64, _: Option<ByteRange>| {
        assert!(allowed_media(u));
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
    let p = local_playlist_with(
        &Url::parse(&cdn("source.m3u8")).unwrap(),
        &scratch,
        &mut 0,
        &mut budget,
        Instant::now() + Duration::from_secs(5),
        0,
        &mut fetch,
    )
    .unwrap();
    let mut output = Vec::new();
    let d = remux(
        &[p],
        &scratch,
        &mut output,
        1000000,
        Instant::now() + Duration::from_secs(10),
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
    let scratch = Scratch::new().unwrap();
    let mut seen = Vec::new();
    let items=[("/test.m3u8",b"#EXTM3U\n#EXT-X-MAP:URI=\"shared.mp4?secret=token\",BYTERANGE=\"4@0\"\n#EXTINF:1,\n#EXT-X-BYTERANGE:3@4\nshared.mp4?secret=token\n#EXTINF:1,\n#EXT-X-BYTERANGE:3\nshared.mp4?secret=token\n#EXT-X-ENDLIST\n".as_slice()),("/shared.mp4",b"INITaaabbbNOTRETRIEVED")];
    let mut fetch = fixture_fetch(&items, &mut seen);
    let mut budget = 10000;
    let p = local_playlist_with(
        &Url::parse(&cdn("test.m3u8")).unwrap(),
        &scratch,
        &mut 0,
        &mut budget,
        Instant::now() + Duration::from_secs(5),
        0,
        &mut fetch,
    )
    .unwrap();
    drop(fetch);
    let local = std::fs::read_to_string(p).unwrap();
    assert!(!local.contains("BYTERANGE") && !local.contains("secret"));
    let mut fragments = std::fs::read_dir(&scratch.0)
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
    let delivery = parse_delivery(ID, &value).unwrap();
    assert!(delivery.mp4_available);
    assert_eq!(delivery.composition, "composed_podcast");
    assert_eq!(delivery.output_stream_count, 1);
    assert_eq!(
        delivery.groups[0].mp4.as_ref().unwrap().path(),
        "/podcast.mp4"
    );
}
