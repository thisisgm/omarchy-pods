//! The `tst_enums` cases, translated, plus the model rows compared with the C++ source.

use super::*;
use cpp_source::{DEVICE_INFO, ENUMS};

/// Every model, in `model_int` order.
const ALL: [Model; 13] = [
    Model::Unknown,
    Model::AirPods1,
    Model::AirPods2,
    Model::AirPods3,
    Model::AirPodsPro,
    Model::AirPodsPro2Lightning,
    Model::AirPodsPro2USBC,
    Model::AirPodsMaxLightning,
    Model::AirPodsMaxUSBC,
    Model::AirPods4,
    Model::AirPods4ANC,
    Model::AirPodsPro3,
    Model::AirPodsMax2,
];

/// One capability flag read out of `Capabilities`.
type Flag = fn(Capabilities) -> bool;

/// The C++ enumerator name, which the Rust variant name copies.
fn cpp_name(model: Model) -> String {
    format!("{model:?}")
}

/// The models a C++ capability function answers true for.
// Sample input: `case AirPodsModel::AirPodsMaxUSBC:` lines above the first `return true;` of a switch.
fn cpp_true_cases(function: &str) -> Vec<String> {
    let body = ENUMS.scope(&format!("inline bool {function}(AirPodsModel model)"));
    let (cases, _) = body
        .split_once("return true;")
        .unwrap_or_else(|| panic!("daemon/enums.h: {function} has no `return true;`"));
    cases
        .split("case AirPodsModel::")
        .skip(1)
        .map(|case| {
            let (name, _) = case
                .split_once(':')
                .unwrap_or_else(|| panic!("daemon/enums.h: {function} has a case without `:`"));
            name.trim().to_owned()
        })
        .collect()
}

/// The models whose flag is set, by C++ name.
fn rust_true_cases(flag: Flag) -> Vec<String> {
    ALL.into_iter()
        .filter(|model| flag(model.capabilities()))
        .map(cpp_name)
        .collect()
}

/// Names in byte order, so a comparison ignores the order of the C++ case labels.
fn sorted(mut names: Vec<String>) -> Vec<String> {
    names.sort();
    names
}

#[test]
fn parse_model_number_matches_apple_support_list() {
    for (number, model) in [
        ("A1523", Model::AirPods1),
        ("A2032", Model::AirPods2),
        ("A2096", Model::AirPodsMaxLightning),
        ("A3184", Model::AirPodsMaxUSBC),
        ("A3454", Model::AirPodsMax2),
        ("A2565", Model::AirPods3),
        ("A3047", Model::AirPodsPro2USBC),
        ("A2931", Model::AirPodsPro2Lightning),
        ("A3053", Model::AirPods4),
        ("A3056", Model::AirPods4ANC),
        ("A3064", Model::AirPodsPro3),
        ("A3063", Model::AirPodsPro3),
        ("A3065", Model::AirPodsPro3),
        // Apple publishes none of these four, so the dropped guesses must not answer Pro 3.
        ("A3066", Model::Unknown),
        ("A3334", Model::Unknown),
        ("A3335", Model::Unknown),
        ("A3336", Model::Unknown),
        ("ZZZZZ", Model::Unknown),
        ("", Model::Unknown),
    ] {
        assert_eq!(Model::from_number(number), model, "{number:?}");
    }
}

#[test]
fn unknown_model_number_is_unknown_not_an_error() {
    for number in [
        "A9999", "a3064", " A3064", "A3064 ", "A30640", "\u{0}", "AirPods",
    ] {
        assert_eq!(Model::from_number(number), Model::Unknown, "{number:?}");
    }
}

#[test]
fn model_display_name_covers_all() {
    assert_eq!(Model::AirPodsPro3.display_name(), "AirPods Pro 3");
    assert_eq!(
        Model::AirPodsPro2USBC.display_name(),
        "AirPods Pro 2 (USB-C)"
    );
    assert_eq!(Model::Unknown.display_name(), "");
}

#[test]
fn model_display_name_exhaustive() {
    for model in ALL.into_iter().skip(1) {
        assert!(
            model.display_name().starts_with("AirPods"),
            "{model:?} is {:?}",
            model.display_name()
        );
    }
    assert_eq!(Model::Unknown.display_name(), "");
    assert_eq!(Model::AirPodsMax2.display_name(), "AirPods Max 2");
    // Both AirPods 4 variants say AirPods 4 on purpose, so a reviewer cannot read the match as a slip.
    assert_eq!(Model::AirPods4.display_name(), "AirPods 4");
    assert_eq!(Model::AirPods4ANC.display_name(), "AirPods 4");
}

#[test]
fn model_ints_are_stable() {
    for (model, int) in [
        (Model::Unknown, 0),
        (Model::AirPods1, 1),
        (Model::AirPods2, 2),
        (Model::AirPods3, 3),
        (Model::AirPodsPro, 4),
        (Model::AirPodsPro2Lightning, 5),
        (Model::AirPodsPro2USBC, 6),
        (Model::AirPodsMaxLightning, 7),
        (Model::AirPodsMaxUSBC, 8),
        (Model::AirPods4, 9),
        (Model::AirPods4ANC, 10),
        (Model::AirPodsPro3, 11),
        (Model::AirPodsMax2, 12),
    ] {
        assert_eq!(model.as_int(), int, "{model:?}");
    }
}

#[test]
fn is_model_headset_only_max() {
    for model in [
        Model::AirPodsMaxLightning,
        Model::AirPodsMaxUSBC,
        Model::AirPodsMax2,
    ] {
        assert!(model.capabilities().is_headset, "{model:?}");
    }
    for model in [
        Model::AirPods1,
        Model::AirPodsPro,
        Model::AirPodsPro2USBC,
        Model::AirPods4ANC,
        Model::Unknown,
    ] {
        assert!(!model.capabilities().is_headset, "{model:?}");
    }
}

#[test]
fn is_pro_series_covers_all_pro_gens() {
    for model in [
        Model::AirPodsPro,
        Model::AirPodsPro2Lightning,
        Model::AirPodsPro2USBC,
        Model::AirPodsPro3,
    ] {
        assert!(model.capabilities().is_pro_series, "{model:?}");
    }
    for model in [
        Model::AirPods1,
        Model::AirPods2,
        Model::AirPods3,
        Model::AirPods4,
        Model::AirPods4ANC,
        Model::AirPodsMaxLightning,
        Model::AirPodsMaxUSBC,
        Model::AirPodsMax2,
        Model::Unknown,
    ] {
        assert!(!model.capabilities().is_pro_series, "{model:?}");
    }
}

#[test]
fn capabilities_match_apple_compare_table() {
    // Columns: noise control, adaptive, conversational awareness, one bud ANC.
    for (model, noise_control, adaptive, conversational_awareness, one_bud_anc) in [
        (Model::AirPods1, false, false, false, false),
        (Model::AirPods2, false, false, false, false),
        (Model::AirPods3, false, false, false, false),
        (Model::AirPods4, false, false, false, false),
        (Model::AirPods4ANC, true, true, true, true),
        (Model::AirPodsPro, true, false, false, true),
        (Model::AirPodsPro2Lightning, true, true, true, true),
        (Model::AirPodsPro2USBC, true, true, true, true),
        (Model::AirPodsPro3, true, true, true, true),
        (Model::AirPodsMaxLightning, true, false, false, false),
        (Model::AirPodsMaxUSBC, true, false, false, false),
        (Model::AirPodsMax2, true, true, true, false),
        // Unknown fails open on modes so an unmapped model keeps what it had, and gains nothing else.
        (Model::Unknown, true, false, false, false),
    ] {
        let c = model.capabilities();
        assert_eq!(c.noise_control, noise_control, "{model:?} noise control");
        assert_eq!(c.adaptive, adaptive, "{model:?} adaptive");
        assert_eq!(
            c.conversational_awareness, conversational_awareness,
            "{model:?} conversational awareness"
        );
        assert_eq!(c.one_bud_anc, one_bud_anc, "{model:?} one bud ANC");
    }
}

#[test]
fn capabilities_never_exceed_noise_control() {
    for model in ALL {
        let c = model.capabilities();
        if !c.noise_control {
            assert!(
                !c.adaptive && !c.conversational_awareness && !c.one_bud_anc,
                "{model:?} has a listening feature without listening modes"
            );
        }
    }
}

#[test]
fn model_numbers_match_the_cpp_model_number_map() {
    // Sample input: `{"A1523", AirPodsModel::AirPods1},` one entry per line, `//` lines between.
    let cpp: Vec<(String, String)> = ENUMS
        .between("modelNumberMap = {", "return modelNumberMap")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("//"))
        .map(|line| {
            let (_, entry) = line
                .split_once("{\"")
                .unwrap_or_else(|| panic!("daemon/enums.h: bad modelNumberMap line `{line}`"));
            let (number, name) = entry
                .split_once("\", AirPodsModel::")
                .unwrap_or_else(|| panic!("daemon/enums.h: bad modelNumberMap line `{line}`"));
            let (name, _) = name
                .split_once('}')
                .unwrap_or_else(|| panic!("daemon/enums.h: bad modelNumberMap line `{line}`"));
            (number.to_owned(), name.to_owned())
        })
        .collect();
    let rust: Vec<(String, String)> = MODEL_NUMBERS
        .iter()
        .map(|&(number, model)| (number.to_owned(), cpp_name(model)))
        .collect();
    assert_eq!(rust, cpp);
    for (number, name) in &cpp {
        assert_eq!(&cpp_name(Model::from_number(number)), name, "{number}");
    }
}

#[test]
fn model_ints_match_the_cpp_enum_order() {
    // Sample input: `{ Unknown, AirPods1, ... // comment ... AirPodsMax2 }`, one enumerator per line.
    let cpp: Vec<&str> = ENUMS
        .scope("enum class AirPodsModel")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && *line != "{" && !line.starts_with("//"))
        .map(|line| line.trim_end_matches(','))
        .collect();
    let rust: Vec<String> = ALL.into_iter().map(cpp_name).collect();
    assert_eq!(rust, cpp);
    for (int, model) in ALL.into_iter().enumerate() {
        assert_eq!(model.as_int(), i64::try_from(int).unwrap(), "{model:?}");
    }
}

#[test]
fn display_names_match_the_cpp_switch() {
    let body = ENUMS.scope("inline QString modelDisplayName(AirPodsModel model)");
    for model in ALL {
        let name = cpp_name(model);
        // Sample input: `case AirPodsModel::AirPods1:   return QStringLiteral("AirPods");`
        let (_, rest) = body
            .split_once(&format!("case AirPodsModel::{name}:"))
            .unwrap_or_else(|| panic!("daemon/enums.h: modelDisplayName has no case {name}"));
        let (statement, _) = rest
            .split_once(';')
            .unwrap_or_else(|| panic!("daemon/enums.h: modelDisplayName case {name} has no `;`"));
        let cpp = match statement.trim() {
            "return QString()" => "",
            literal => literal
                .strip_prefix("return QStringLiteral(\"")
                .and_then(|rest| rest.strip_suffix("\")"))
                .unwrap_or_else(|| {
                    panic!("daemon/enums.h: modelDisplayName case {name} is `{literal}`")
                }),
        };
        assert_eq!(model.display_name(), cpp, "{name}");
    }
}

#[test]
fn capabilities_match_the_cpp_switches() {
    let flags: [(&str, Flag); 4] = [
        ("supportsNoiseControl", |c| c.noise_control),
        ("supportsOneBudANC", |c| c.one_bud_anc),
        ("isProSeriesAirPods", |c| c.is_pro_series),
        ("isModelHeadset", |c| c.is_headset),
    ];
    for (function, flag) in flags {
        assert_eq!(
            sorted(rust_true_cases(flag)),
            sorted(cpp_true_cases(function)),
            "{function}"
        );
    }
    let h2 = sorted(cpp_true_cases("hasH2ListeningFeatures"));
    let delegates: [(&str, Flag); 2] = [
        ("supportsAdaptiveAudio", |c| c.adaptive),
        ("supportsConversationalAwareness", |c| {
            c.conversational_awareness
        }),
    ];
    for (function, flag) in delegates {
        let body = ENUMS.scope(&format!("inline bool {function}(AirPodsModel model)"));
        assert_eq!(
            body.trim().trim_start_matches('{').trim(),
            "return hasH2ListeningFeatures(model);",
            "{function}"
        );
        assert_eq!(sorted(rust_true_cases(flag)), h2, "{function}");
    }
    // Sample input: `return model != AirPodsModel::AirPodsPro3;`
    let without_off = ENUMS.find(
        Some("inline bool supportsNoiseOff(AirPodsModel model)"),
        "return model != AirPodsModel::",
        ';',
    );
    let rust_without_off: Vec<String> = ALL
        .into_iter()
        .filter(|model| !model.capabilities().noise_off)
        .map(cpp_name)
        .collect();
    assert_eq!(rust_without_off, [without_off]);
}

#[test]
fn default_model_matches_the_cpp_device_info() {
    // Sample input: `AirPodsModel m_model = AirPodsModel::Unknown;`
    let initial = DEVICE_INFO.find(None, "AirPodsModel m_model = AirPodsModel::", ';');
    assert_eq!(cpp_name(Model::default()), initial);
    // Sample input: `setModel(AirPodsModel::Unknown);` inside `void reset()`.
    let reset = DEVICE_INFO.find(Some("void reset()"), "setModel(AirPodsModel::", ')');
    assert_eq!(cpp_name(Model::default()), reset);
}
