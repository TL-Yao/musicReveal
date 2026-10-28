use crate::{strip_timestamps, Platform, SongId, TrackMeta};

#[test]
fn strip_timestamps_drops_tags_meta_and_blank_lines() {
    let lrc =
        "[ti:]\n[ar:hanser]\n[offset:0]\n\n[00:01.00][00:30.00]副歌\n  \n[00:02.50]正文 [和声]\n\n";
    assert_eq!(strip_timestamps(lrc), "副歌\n正文 [和声]");
}

fn fx(name: &str) -> Vec<u8> {
    let p = format!(
        "{}/tests/fixtures/enrich/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read(&p).unwrap_or_else(|e| panic!("{p}: {e}"))
}

fn blank(platform: Platform, id: SongId) -> TrackMeta {
    TrackMeta {
        title: Some("文件标题".into()),
        artists: vec!["文件歌手".into()],
        album: Some("文件专辑".into()),
        album_artist: Some("文件专辑艺人".into()),
        year: Some(1999),
        track: Some(9),
        disc: Some(9),
        cover: Some(vec![1, 2, 3]),
        lyrics: None,
        song_id: Some(id),
        platform,
    }
}

fn qq() -> TrackMeta {
    blank(Platform::Qq, SongId::QqMid("0015IeIl3kBP3P".into()))
}

#[test]
fn qq_song_fills_fields_and_returns_album_mid() {
    let mut m = qq();
    let mid = crate::enrich::qq_song(&fx("qq_song_0015IeIl3kBP3P.json"), &mut m).unwrap();
    assert_eq!(mid.as_deref(), Some("003PDOea18kOjf"));
    assert_eq!(m.title.as_deref(), Some("二次元凶"));
    assert_eq!(m.artists, ["hanser"]);
    assert_eq!(m.album.as_deref(), Some("二次元凶"));
    assert_eq!((m.year, m.track, m.disc), (Some(2025), Some(1), Some(1)));
    // 曲库没给专辑艺人：保留原值，不拿歌曲歌手顶替
    assert_eq!(m.album_artist.as_deref(), Some("文件专辑艺人"));
    assert_eq!(m.cover, Some(vec![1, 2, 3]));
}

#[test]
fn qq_song_missing_fields_keep_original_values() {
    let body = br#"{"code":0,"data":[{"name":"N","singer":[],"index_album":0,
        "album":{"name":"","singer":[{"name":"A"},{"name":"B"}],"time_public":"2019-01-01"}}]}"#;
    let mut m = qq();
    assert_eq!(crate::enrich::qq_song(body, &mut m).unwrap(), None);
    assert_eq!(m.title.as_deref(), Some("N"));
    assert_eq!(m.artists, ["文件歌手"]);
    assert_eq!(m.album.as_deref(), Some("文件专辑"));
    assert_eq!(m.album_artist.as_deref(), Some("A, B"));
    assert_eq!((m.year, m.track, m.disc), (Some(2019), Some(9), Some(9)));

    let mut m = qq();
    assert!(crate::enrich::qq_song(br#"{"code":0,"data":[]}"#, &mut m).is_err());
    assert_eq!(m, qq());
}

#[test]
fn qq_lyric_from_recorded_response() {
    let l = crate::enrich::qq_lyric(&fx("qq_lyric_0015IeIl3kBP3P.json"))
        .unwrap()
        .unwrap();
    assert!(l.lrc.starts_with("[ti:]\n"), "{}", l.lrc);
    assert!(l.lrc.contains("[00:16.56]穿梭在人群\n"));
    assert_eq!(l.translated_lrc, None);
}

#[test]
fn qq_lyric_decodes_html_entities_and_keeps_translation() {
    let body = r#"{"retcode":0,"lyric":"[ti&#58;x]\n[00:01.00]It&apos;s &amp; &#x4E2D;&#25991;&nbsp;&lt;3&gt; &unknown;","trans":"[00:01.00]&quot;译&quot;"}"#;
    let l = crate::enrich::qq_lyric(body.as_bytes()).unwrap().unwrap();
    assert_eq!(l.lrc, "[ti:x]\n[00:01.00]It's & 中文 <3> &unknown;");
    assert_eq!(l.translated_lrc.as_deref(), Some("[00:01.00]\"译\""));
}

#[test]
fn qq_instrumental_or_credit_only_lyrics_are_none() {
    for lyric in [
        "",
        "[00:00.00]此歌曲为没有填词的纯音乐，请您欣赏",
        "[ti:x]\n[00:00.00]此歌曲为没有填词的纯音乐，请您欣赏\n",
        "[00:00.00]作词 : 无\n[00:01.00]作曲&#58;Yunomi\n[00:02.00]编曲 Yunomi",
    ] {
        let body = serde_json::json!({"retcode": 0, "lyric": lyric, "trans": ""}).to_string();
        assert_eq!(
            crate::enrich::qq_lyric(body.as_bytes()).unwrap(),
            None,
            "{lyric}"
        );
    }
}
