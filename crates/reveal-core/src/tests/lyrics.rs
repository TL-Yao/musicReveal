use crate::convert::{merge_lrc, strip_timestamps};
use crate::Lyrics;

fn merge(lrc: &str, tr: Option<&str>) -> String {
    merge_lrc(&Lyrics {
        lrc: lrc.into(),
        translated_lrc: tr.map(Into::into),
    })
}

#[test]
fn translation_matches_by_parsed_time_not_by_text() {
    assert_eq!(
        merge(
            "[00:01.00]一\n[00:02.5]二\n[01:03.120]三",
            Some("[00:01.0]one\n[00:02.500]two\n[01:03.12]three")
        ),
        "[00:01.00]一\n[00:01.00]one\n[00:02.5]二\n[00:02.5]two\n[01:03.120]三\n[01:03.120]three\n"
    );
}

#[test]
fn multi_timestamp_lines_are_expanded_with_their_translations() {
    assert_eq!(
        merge(
            "[ti:x]\n[00:01.00][00:30.00]副歌",
            Some("[00:30.00]chorus2\n[00:01.00]chorus1")
        ),
        "[ti:x]\n[00:01.00]副歌\n[00:01.00]chorus1\n[00:30.00]副歌\n[00:30.00]chorus2\n"
    );
    assert_eq!(strip_timestamps("[00:01.00][00:30.00]副歌\n[ar:y]"), "副歌");
}

#[test]
fn plain_lines_with_brackets_do_not_panic() {
    assert_eq!(
        strip_timestamps("中文 [和声]\n[00:01]词"),
        "中文 [和声]\n词"
    );
    assert_eq!(merge("中文 [和声]", Some("中文 [和声]")), "中文 [和声]\n");
}

#[test]
fn arbitrary_input_never_panics() {
    let alphabet: Vec<char> = "[]:.0123456789中a \n\t]".chars().collect();
    let mut x: u64 = 0x2545_F491_4F6C_DD1D;
    for _ in 0..3000 {
        let mut s = String::new();
        for _ in 0..(x % 40) {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            s.push(alphabet[(x % alphabet.len() as u64) as usize]);
        }
        let _ = strip_timestamps(&s);
        let _ = merge(&s, Some(&s));
    }
}

#[test]
fn extreme_time_tags_are_not_timestamps_and_never_panic() {
    let max = u64::MAX;
    for l in [
        format!("[{max}:00]词"),
        format!("[{max}:{max}.999]词"),
        "[307445734561825:59.999]词".to_string(),
        format!("[00:{max}]词"),
        "[99999999999999999999999:00]词".to_string(),
    ] {
        assert_eq!(strip_timestamps(&l), l, "{l}");
        assert_eq!(merge(&l, Some(&l)), format!("{l}\n"));
    }
    // 不溢出的大值仍是时间标签
    assert_eq!(strip_timestamps("[1000000:00.00]词"), "词");

    // 长数字串模糊：任意位数的分/秒/小数都不 panic
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = |n: u64| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x % n
    };
    for _ in 0..3000 {
        let mut s = String::from("[");
        for part in [":", ".", "]词"] {
            for _ in 0..next(30) {
                s.push(char::from(b'0' + next(10) as u8));
            }
            s += part;
        }
        let _ = strip_timestamps(&s);
        let _ = merge(&s, Some(&s));
    }
}
