use super::Tmp;
use crate::creds::load_in;
use crate::{credentials_status, load_credentials, Credentials, QqStatus};
use plist::{Dictionary, Uid, Value};
use std::path::{Path, PathBuf};

const NEW: &str = "Library/Preferences/com.tencent.QQMusicMac.plist";
const OLD: &str = "Library/Containers/com.tencent.QQMusicMac/Data/Library/Preferences/com.tencent.QQMusicMac.plist";

/// NSKeyedArchiver 形状：字段值是指向 `$objects` 的 UID。
fn archive(user: &[(&str, Value)]) -> Vec<u8> {
    let mut objects = vec![Value::String("$null".into()), Value::Boolean(false)];
    let mut d = Dictionary::new();
    for (k, v) in user {
        d.insert((*k).into(), Value::Uid(Uid::new(objects.len() as u64)));
        objects.push(v.clone());
    }
    objects[1] = Value::Dictionary(d);
    let mut a = Dictionary::new();
    a.insert("$archiver".into(), Value::String("NSKeyedArchiver".into()));
    a.insert("$objects".into(), Value::Array(objects));
    let mut bytes = Vec::new();
    Value::Dictionary(a).to_writer_binary(&mut bytes).unwrap();
    bytes
}

fn write_plist(home: &Path, rel: &str, archived: Option<Vec<u8>>) -> PathBuf {
    let p = home.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    let mut outer = Dictionary::new();
    outer.insert("Other".into(), Value::Boolean(true));
    if let Some(a) = archived {
        outer.insert("AutoLoginUserInfo".into(), Value::Data(a));
    }
    Value::Dictionary(outer).to_file_binary(&p).unwrap();
    p
}

fn logged(authst: &str, uin: i64) -> Option<Vec<u8>> {
    Some(archive(&[
        ("strAuthst", Value::String(authst.into())),
        ("nUserId", Value::Integer(uin.into())),
        ("loginType", Value::String("2".into())),
    ]))
}

fn no_sys_apps(home: &Path) -> PathBuf {
    home.join("no-system-apps")
}

#[test]
fn logged_in_resolves_uid_references_and_masks_uin() {
    let t = Tmp::new("cred-in");
    write_plist(&t.0, NEW, logged("secret-authst", 12345678));
    assert_eq!(
        credentials_status(&t.0),
        QqStatus::LoggedIn {
            masked_uin: "12****78".into()
        }
    );
    let c = load_credentials(&t.0).unwrap();
    assert_eq!(
        (c.uin.as_str(), c.authst.as_str(), c.login_type.as_str()),
        ("12345678", "secret-authst", "2")
    );
}

#[test]
fn falls_back_to_user_account_and_default_login_type() {
    let t = Tmp::new("cred-fb");
    write_plist(
        &t.0,
        NEW,
        Some(archive(&[
            ("strAuthst", Value::String("a".into())),
            ("strUserAccount", Value::String("987654".into())),
        ])),
    );
    let c = load_credentials(&t.0).unwrap();
    assert_eq!((c.uin.as_str(), c.login_type.as_str()), ("987654", "3"));
}

#[test]
fn plist_without_authst_is_logged_out() {
    let t = Tmp::new("cred-out");
    write_plist(&t.0, NEW, None);
    assert_eq!(credentials_status(&t.0), QqStatus::LoggedOut);
    write_plist(
        &t.0,
        NEW,
        Some(archive(&[
            ("strAuthst", Value::String("".into())),
            ("nUserId", Value::Integer(1.into())),
        ])),
    );
    assert_eq!(credentials_status(&t.0), QqStatus::LoggedOut);
}

#[test]
fn app_installed_without_plist_is_logged_out_else_not_installed() {
    let t = Tmp::new("cred-app");
    assert_eq!(
        load_in(&t.0, &no_sys_apps(&t.0)).unwrap_err(),
        QqStatus::NotInstalled
    );
    std::fs::create_dir_all(t.0.join("Applications/QQMusic.app")).unwrap();
    assert_eq!(
        load_in(&t.0, &no_sys_apps(&t.0)).unwrap_err(),
        QqStatus::LoggedOut
    );
}

#[test]
fn unreadable_container_plist_needs_auth() {
    use std::os::unix::fs::PermissionsExt;
    let t = Tmp::new("cred-auth");
    let p = write_plist(&t.0, OLD, logged("a", 11111));
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o000)).unwrap();
    let st = credentials_status(&t.0);
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(st, QqStatus::NeedsAuth);
}

#[test]
fn debug_output_hides_secrets() {
    let c = Credentials {
        uin: "1234567890".into(),
        authst: "Q_H_L_supersecret".into(),
        login_type: "3".into(),
    };
    let s = format!("{c:?} {c:#?}");
    assert!(
        !s.contains("1234567890") && !s.contains("supersecret"),
        "{s}"
    );
}

fn set_age(p: &Path, secs: u64) {
    let when = std::time::SystemTime::now() - std::time::Duration::from_secs(secs);
    std::fs::File::options()
        .write(true)
        .open(p)
        .unwrap()
        .set_modified(when)
        .unwrap();
}

fn chmod(p: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
}

#[test]
fn valid_new_plist_wins_even_if_container_plist_is_newer() {
    let t = Tmp::new("cred-new-first");
    let new = write_plist(&t.0, NEW, logged("a", 11111111));
    write_plist(&t.0, OLD, logged("b", 22222222));
    set_age(&new, 3600);
    assert_eq!(load_credentials(&t.0).unwrap().uin, "11111111");
}

#[test]
fn valid_new_plist_wins_over_newer_unreadable_container_plist() {
    let t = Tmp::new("cred-newer-locked");
    let new = write_plist(&t.0, NEW, logged("a", 11111111));
    let old = write_plist(&t.0, OLD, logged("b", 22222222));
    set_age(&new, 3600);
    chmod(&old, 0o000);
    let st = credentials_status(&t.0);
    let loaded = load_credentials(&t.0).map(|c| c.uin);
    chmod(&old, 0o644);
    assert_eq!(
        st,
        QqStatus::LoggedIn {
            masked_uin: "11****11".into()
        }
    );
    assert_eq!(loaded.unwrap(), "11111111");
}

#[test]
fn logged_out_new_plist_falls_back_to_container_plist() {
    let t = Tmp::new("cred-fallback");
    write_plist(&t.0, NEW, None);
    write_plist(&t.0, OLD, logged("b", 22222222));
    assert_eq!(load_credentials(&t.0).unwrap().uin, "22222222");
}

#[test]
fn readable_container_plist_without_login_is_logged_out() {
    let t = Tmp::new("cred-old-out");
    write_plist(&t.0, OLD, None);
    assert_eq!(
        load_in(&t.0, &no_sys_apps(&t.0)).unwrap_err(),
        QqStatus::LoggedOut
    );
}

#[test]
fn inaccessible_empty_container_dir_needs_auth() {
    let t = Tmp::new("cred-empty-locked");
    let dir = t.0.join("Library/Containers/com.tencent.QQMusicMac");
    std::fs::create_dir_all(&dir).unwrap();
    chmod(&dir, 0o000);
    let st = load_in(&t.0, &no_sys_apps(&t.0));
    chmod(&dir, 0o755);
    assert_eq!(st.unwrap_err(), QqStatus::NeedsAuth);
}

#[test]
fn unreadable_new_plist_and_locked_container_plist_needs_auth() {
    let t = Tmp::new("cred-both-locked");
    write_plist(&t.0, NEW, None);
    let old = write_plist(&t.0, OLD, logged("b", 22222222));
    chmod(&old, 0o000);
    let st = credentials_status(&t.0);
    chmod(&old, 0o644);
    assert_eq!(st, QqStatus::NeedsAuth);
}
