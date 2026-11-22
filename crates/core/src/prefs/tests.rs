//! Support tests: the settings file and the old C++ file read as text, with no user path of their own.

use cpp_source::{MAIN, MEDIA_CONTROLLER};
use proptest::prelude::*;

use super::*;

fn every_prefs() -> Vec<Prefs> {
    EarDetectionBehavior::ALL
        .into_iter()
        .flat_map(|ear_detection| {
            [true, false].map(|notifications| Prefs {
                ear_detection,
                notifications,
            })
        })
        .collect()
}

#[test]
fn the_defaults_are_the_cpp_defaults() {
    let ear = MAIN.between("\"earDetection/setting\", ", ")");
    assert_eq!(
        ear,
        "MediaController::EarDetectionBehavior::PauseWhenOneRemoved"
    );
    let notices = MAIN.between("\"notifications/enabled\", ", ")");
    assert_eq!(notices, "true");
    let defaults = Prefs::default();
    assert_eq!(
        defaults.ear_detection,
        EarDetectionBehavior::PauseWhenOneRemoved
    );
    assert!(defaults.notifications);
}

#[test]
fn render_then_parse_is_the_identity() {
    for prefs in every_prefs() {
        assert_eq!(parse(&render(prefs)), Ok(prefs), "{prefs:?}");
    }
}

#[test]
fn the_sample_parses() {
    let sample = r#"{"version":1,"ear_detection":"one","notifications":true}"#;
    assert_eq!(parse(sample), Ok(Prefs::default()));
}

#[test]
fn a_cut_or_wrong_file_is_refused() {
    let whole = render(Prefs::default());
    for cut in 0..whole.trim_end().len() {
        let part = whole.get(..cut).unwrap();
        assert!(parse(part).is_err(), "{part:?}");
    }
    for (text, error) in [
        ("[]", PrefsError::NotAnObject),
        ("null", PrefsError::NotAnObject),
        (
            r#"{"ear_detection":"one","notifications":true}"#,
            PrefsError::Version,
        ),
        (
            r#"{"version":2,"ear_detection":"one","notifications":true}"#,
            PrefsError::Version,
        ),
        (
            r#"{"version":"1","ear_detection":"one","notifications":true}"#,
            PrefsError::Version,
        ),
        (
            r#"{"version":1,"ear_detection":"two","notifications":true}"#,
            PrefsError::EarDetection,
        ),
        (
            r#"{"version":1,"ear_detection":1,"notifications":true}"#,
            PrefsError::EarDetection,
        ),
        (
            r#"{"version":1,"ear_detection":"one","notifications":"true"}"#,
            PrefsError::Notifications,
        ),
        (
            r#"{"version":1,"ear_detection":"one"}"#,
            PrefsError::Notifications,
        ),
    ] {
        assert_eq!(parse(text), Err(error), "{text}");
    }
}

#[test]
fn an_oversized_document_still_parses_without_harm() {
    let mut padded = render(Prefs::default());
    padded.push_str(&" ".repeat(1 << 20));
    assert_eq!(parse(&padded), Ok(Prefs::default()));
    assert!(parse(&"[".repeat(1 << 16)).is_err());
}

#[test]
fn the_old_keys_are_the_cpp_keys() {
    for (group, key) in [OLD_EAR_DETECTION, OLD_NOTIFICATIONS] {
        let quoted = format!("\"{group}/{key}\"");
        assert!(MAIN.text().contains(&quoted), "{quoted}");
    }
}

#[test]
fn the_old_numbers_are_the_cpp_enumeration() {
    let enumerators = MEDIA_CONTROLLER.enumerators("enum EarDetectionBehavior");
    assert_eq!(enumerators.len(), EarDetectionBehavior::ALL.len());
    for (name, number) in enumerators {
        let read = from_old_conf(&format!("[earDetection]\nsetting={number}\n"));
        assert_eq!(format!("{:?}", read.ear_detection), name);
    }
}

#[test]
fn the_default_values_are_read_not_fallen_back_to() {
    let disabled = from_old_conf("[earDetection]\nsetting=2\n");
    assert_eq!(disabled.ear_detection, EarDetectionBehavior::Disabled);
    let ear = from_old_conf("[earDetection]\nsetting=2\n[earDetection]\nsetting=0\n");
    assert_eq!(ear.ear_detection, EarDetectionBehavior::PauseWhenOneRemoved);
    let ear = from_old_conf("[earDetection]\nsetting=0\n[earDetection]\nsetting=2\n");
    assert_eq!(ear.ear_detection, EarDetectionBehavior::Disabled);
    assert!(!from_old_conf("[notifications]\nenabled=false\n").notifications);
    let notices = from_old_conf("[notifications]\nenabled=false\n[notifications]\nenabled=true\n");
    assert!(notices.notifications);
    let notices = from_old_conf("[notifications]\nenabled=true\n[notifications]\nenabled=false\n");
    assert!(!notices.notifications);
}

#[test]
fn the_old_file_gives_both_keys() {
    let text =
        "[General]\nphoneMac=\n\n[earDetection]\nsetting=1\n\n[notifications]\nenabled=false\n";
    let read = from_old_conf(text);
    assert_eq!(
        read.ear_detection,
        EarDetectionBehavior::PauseWhenBothRemoved
    );
    assert!(!read.notifications);
    let spaced = from_old_conf("[earDetection]\r\n setting = 2 \r\n");
    assert_eq!(spaced.ear_detection, EarDetectionBehavior::Disabled);
}

#[test]
fn anything_unread_in_the_old_file_is_the_default() {
    for text in [
        "",
        "[earDetection]\nsett",
        "[earDetection]\nsetting=",
        "[earDetection]\nsetting=3",
        "[earDetection]\nsetting=-1",
        "[earDetection]\nsetting=256",
        "[earDetection]\nsetting=one",
        "[notifications]\nenabled=maybe",
        "[notifications]\nenabled=False",
        "[General]\nsetting=1\nenabled=false",
        "setting=1\nenabled=false",
        "[earDetection\nsetting=1",
        "[notifications]\nenabled",
    ] {
        assert_eq!(from_old_conf(text), Prefs::default(), "{text:?}");
    }
    let huge = format!("[earDetection]\nsetting={}\n", "9".repeat(1 << 16));
    assert_eq!(from_old_conf(&huge), Prefs::default());
}

proptest! {
    #[test]
    fn any_text_parses_or_is_refused(text in ".{0,256}") {
        if let Ok(prefs) = parse(&text) {
            prop_assert_eq!(parse(&render(prefs)), Ok(prefs));
        }
    }

    #[test]
    fn any_old_file_gives_some_prefs(text in "(\\[[a-zA-Z]{0,14}\\]\n|[a-z]{0,8}=.{0,4}\n|.{0,8}\n){0,16}") {
        let read = from_old_conf(&text);
        prop_assert_eq!(parse(&render(read)), Ok(read));
    }

    #[test]
    fn any_bytes_read_as_text_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
        let text = String::from_utf8_lossy(&bytes);
        prop_assert!(parse(&text).is_err() || text.contains("version"));
        let read = from_old_conf(&text);
        prop_assert_eq!(parse(&render(read)), Ok(read));
    }
}
