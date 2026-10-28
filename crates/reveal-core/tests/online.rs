//! 联网补全：`cargo test -p reveal-core -- --ignored`。只查公开曲目元数据。

use reveal_core::{Enricher, OnlineEnricher, Platform, SongId, TrackMeta};

fn meta(id: SongId, platform: Platform) -> TrackMeta {
    TrackMeta {
        title: None,
        artists: vec![],
        album: None,
        album_artist: None,
        year: None,
        track: None,
        disc: None,
        cover: None,
        lyrics: None,
        song_id: Some(id),
        platform,
    }
}

#[tokio::test]
#[ignore]
async fn online_qq_by_mid() {
    let mut m = meta(SongId::QqMid("0015IeIl3kBP3P".into()), Platform::Qq);
    OnlineEnricher::new().enrich(&mut m).await.unwrap();
    assert_eq!(m.album.as_deref(), Some("二次元凶"));
    assert_eq!(m.year, Some(2025));
    assert!(m.cover.as_ref().is_some_and(|c| c.len() > 1024));
    assert!(m.lyrics.is_some());
}

#[tokio::test]
#[ignore]
async fn online_netease_by_id() {
    let e = OnlineEnricher::new();
    let mut m = meta(SongId::Netease(1352449924), Platform::Netease);
    e.enrich(&mut m).await.unwrap();
    assert_eq!((m.track, m.disc, m.year), (Some(4), Some(1), Some(2017)));
    assert_eq!(m.album_artist.as_deref(), Some("著小生zoki"));
    assert!(m.cover.is_some());
    assert_eq!(m.lyrics, None);

    let mut m = meta(SongId::Netease(1982798727), Platform::Netease);
    e.enrich(&mut m).await.unwrap();
    assert_eq!(m.album.as_deref(), Some("一抹憨色"));
    assert!(m.cover.is_some() && m.lyrics.is_some());
}

#[tokio::test]
async fn no_song_id_is_a_no_op() {
    let mut m = meta(SongId::Netease(1), Platform::Netease);
    m.song_id = None;
    let before = m.clone();
    OnlineEnricher::new().enrich(&mut m).await.unwrap();
    assert_eq!(m, before);
}
