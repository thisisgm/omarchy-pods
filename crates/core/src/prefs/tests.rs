//! Support tests: the settings file and the old C++ file read as text, with no user path of their own.

use proptest::prelude::*;

use super::*;
use crate::state::MicCode;

fn every_prefs() -> Vec<Prefs> {
    EarDetectionBehavior::ALL
        .into_iter()
        .flat_map(|ear_detection| {
            [true, false].map(move |notifications| (ear_detection, notifications))
        })
        .flat_map(|(ear_detection, notifications)| {
            MicPref::ALL.map(move |microphone| (ear_detection, notifications, microphone))
        })
        .flat_map(|(ear_detection, notifications, microphone)| {
            model_lists().map(|mic_models| Prefs {
                ear_detection,
                notifications,
                microphone,
                mic_models,
            })
        })
        .collect()
}

/// None, one, and as many as are kept.
fn model_lists() -> [MicModels; 3] {
    let mut one = MicModels::default();
    one.confirm("A3063");
    let mut full = MicModels::default();
    for number in 0..MAX_CONFIRMED_MODELS {
        full.confirm(&format!("B{number:04}"));
    }
    [MicModels::default(), one, full]
}

#[test]
fn defaults_enable_notifications_and_pause_when_one_is_removed() {
    let defaults = Prefs::default();
    assert_eq!(
        defaults.ear_detection,
        EarDetectionBehavior::PauseWhenOneRemoved
    );
    assert!(defaults.notifications);
    assert_eq!(defaults.microphone, MicPref::Auto);
}

#[test]
fn each_microphone_word_reads_back_and_no_other_word_does() {
    for (preference, word) in [
        (MicPref::Auto, "auto"),
        (MicPref::Always, "always"),
        (MicPref::Off, "off"),
    ] {
        assert_eq!(preference.word(), word);
        assert_eq!(MicPref::from_word(word), Some(preference));
    }
    for word in ["", "Auto", "on", "true", "off ", "idle"] {
        assert_eq!(MicPref::from_word(word), None, "{word:?}");
    }
}

#[test]
fn a_file_without_the_microphone_key_reads_auto_and_keeps_its_other_choices() {
    let read = parse(r#"{"version":1,"ear_detection":"both","notifications":false}"#);
    let expected = Prefs {
        ear_detection: EarDetectionBehavior::PauseWhenBothRemoved,
        notifications: false,
        microphone: MicPref::Auto,
        mic_models: MicModels::default(),
    };
    assert!(
        render(&expected)
            .starts_with(r#"{"ear_detection":"both","mic_models":[],"microphone":"auto","#)
    );
    assert!(render(&expected).contains(r#""version":1"#));
    assert_eq!(read, Ok(expected));
}

#[test]
fn a_fresh_state_reads_the_kept_microphone_on_both_keys() {
    for prefs in every_prefs() {
        let state = prefs.clone().into_state();
        let resting = if prefs.microphone == MicPref::Off {
            MicCode::Off
        } else {
            MicCode::Idle
        };
        assert_eq!(state.microphone.preference(), prefs.microphone, "{prefs:?}");
        assert_eq!(state.microphone.code(), resting, "{prefs:?}");
    }
}

#[test]
fn render_then_parse_is_the_identity() {
    for prefs in every_prefs() {
        assert_eq!(parse(&render(&prefs)), Ok(prefs.clone()), "{prefs:?}");
    }
}

#[test]
fn the_sample_parses() {
    let sample = r#"{"version":1,"ear_detection":"one","notifications":true}"#;
    assert_eq!(parse(sample), Ok(Prefs::default()));
}

#[test]
fn a_cut_or_wrong_file_is_refused() {
    let whole = render(&Prefs::default());
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
        (
            r#"{"version":1,"ear_detection":"one","notifications":true,"microphone":"loud"}"#,
            PrefsError::Microphone,
        ),
        (
            r#"{"version":1,"ear_detection":"one","notifications":true,"microphone":true}"#,
            PrefsError::Microphone,
        ),
        (
            r#"{"version":1,"ear_detection":"one","notifications":true,"microphone":null}"#,
            PrefsError::Microphone,
        ),
    ] {
        assert_eq!(parse(text), Err(error), "{text}");
    }
}

#[test]
fn an_oversized_document_still_parses_without_harm() {
    let mut padded = render(&Prefs::default());
    padded.push_str(&" ".repeat(1 << 20));
    assert_eq!(parse(&padded), Ok(Prefs::default()));
    assert!(parse(&"[".repeat(1 << 16)).is_err());
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
            prop_assert_eq!(parse(&render(&prefs)), Ok(prefs));
        }
    }

    #[test]
    fn any_old_file_gives_some_prefs(text in "(\\[[a-zA-Z]{0,14}\\]\n|[a-z]{0,8}=.{0,4}\n|.{0,8}\n){0,16}") {
        let read = from_old_conf(&text);
        prop_assert_eq!(parse(&render(&read)), Ok(read));
    }

    #[test]
    fn any_bytes_read_as_text_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
        let text = String::from_utf8_lossy(&bytes);
        prop_assert!(parse(&text).is_err() || text.contains("version"));
        let read = from_old_conf(&text);
        prop_assert_eq!(parse(&render(&read)), Ok(read));
    }
}

#[test]
fn a_file_without_mic_models_reads_an_empty_list() {
    let read =
        parse(r#"{"version":1,"ear_detection":"one","notifications":true,"microphone":"always"}"#);
    assert_eq!(read.map(|prefs| prefs.mic_models), Ok(MicModels::default()));
}

#[test]
fn either_primary_bud_uses_the_same_mic_model_confirmation() {
    for (kept, reported) in [("A3064", "A3063"), ("A3063", "A3064")] {
        let file = json!({
            "version": 1,
            "ear_detection": "one",
            "notifications": true,
            "mic_models": [kept],
        });
        let mut prefs = parse(&file.to_string()).unwrap();
        assert!(
            prefs.mic_models.contains(reported),
            "kept {kept}, reported {reported}"
        );
        prefs.mic_models.confirm(reported);
        assert_eq!(parse(&file.to_string()), Ok(prefs));
    }
    let mut models = MicModels::default();
    models.confirm("A3064");
    assert_eq!(models.0, ["A3063"]);
}

#[test]
fn a_bad_model_list_is_refused_and_a_good_one_reads_back() {
    let full: Vec<String> = (0..MAX_CONFIRMED_MODELS)
        .map(|number| format!("B{number:04}"))
        .collect();
    let mut over = full.clone();
    over.push("C0000".to_owned());
    let longest = "Z".repeat(MODEL_NUMBER_MAX);
    let too_long = "Z".repeat(MODEL_NUMBER_MAX + 1);
    let file = |models: Value| {
        parse(&json!({"version": 1, "ear_detection": "one", "notifications": true, "mic_models": models}).to_string())
    };
    for bad in [
        json!(over),
        json!([""]),
        json!([too_long]),
        json!(["a3063"]),
        json!(["A30 63"]),
        json!(["A3063", "A3063"]),
        json!([3063]),
        json!([null]),
        json!("A3063"),
        json!({}),
    ] {
        assert_eq!(file(bad.clone()), Err(PrefsError::MicModels), "{bad}");
    }
    for good in [
        json!([]),
        json!(full),
        json!([longest, "A3063"]),
        json!(["A3063", "A3064"]),
    ] {
        let prefs = file(good.clone()).unwrap();
        assert!(
            render(&prefs).contains(&format!(r#""mic_models":{good}"#)),
            "{good}"
        );
    }
}

#[test]
fn confirm_keeps_a_new_number_once_and_drops_the_oldest_past_the_cap() {
    let mut models = MicModels::default();
    for number in ["", "a3063", "A3063", "A3063", "123456789"] {
        models.confirm(number);
    }
    assert_eq!(models, model_lists()[1]);
    let mut full = model_lists()[2].clone();
    full.confirm("C0000");
    assert!(!full.contains("B0000") && full.contains("B0001") && full.contains("C0000"));
    let text = render(&Prefs {
        mic_models: full,
        ..Prefs::default()
    });
    assert!(text.contains(r#""mic_models":["B0001","#), "{text}");
    assert!(text.contains(r#""B0015","C0000"]"#), "{text}");
}
