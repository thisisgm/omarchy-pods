//! The `tst_enums` cases, translated, plus the model rows compared with the C++ source.

use super::*;
use cpp_source::{DEVICE_INFO, ENUMS};
use proptest::prelude::*;

type Flag = fn(Capabilities) -> bool;

/// The C++ enumerator name, which the Rust variant name copies.
fn cpp_name(model: Model) -> String {
    format!("{model:?}")
}

/// Sample input: `case AirPodsModel::AirPodsMaxUSBC:` lines above a switch's first `return true;`.
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

fn rust_true_cases(flag: Flag) -> Vec<String> {
    Model::ALL
        .iter()
        .copied()
        .filter(|model| flag(model.capabilities()))
        .map(cpp_name)
        .collect()
}

/// So a comparison ignores the order of the C++ case labels.
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
    for &model in Model::ALL.iter().skip(1) {
        assert!(
            model.display_name().starts_with("AirPods"),
            "{model:?} is {:?}",
            model.display_name()
        );
    }
    assert_eq!(Model::Unknown.display_name(), "");
    assert_eq!(Model::AirPodsMax2.display_name(), "AirPods Max 2");
    // Both AirPods 4 variants say AirPods 4 on purpose, so the match is not a slip.
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
    let rows = [
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
        // Unknown fails open on modes: an unmapped model keeps what it had and gains nothing else.
        (Model::Unknown, true, false, false, false),
    ];
    for (model, noise_control, adaptive, conversational_awareness, one_bud_anc) in rows {
        let c = model.capabilities();
        assert_eq!(c.noise_control, noise_control, "{model:?} noise control");
        assert_eq!(c.adaptive, adaptive, "{model:?} adaptive");
        assert_eq!(
            c.conversational_awareness, conversational_awareness,
            "{model:?} conversational awareness"
        );
        assert_eq!(c.one_bud_anc, one_bud_anc, "{model:?} one bud ANC");
    }
    let mut listed: Vec<Model> = rows.iter().map(|&(model, ..)| model).collect();
    listed.sort_by_key(|model| model.as_int());
    assert_eq!(listed, Model::ALL, "every model has one row here");
}

#[test]
fn only_unknown_has_no_model_number() {
    for &model in Model::ALL {
        let rows = MODEL_NUMBERS
            .iter()
            .filter(|&&(_, row)| row == model)
            .count();
        assert_eq!(
            rows > 0,
            model != Model::Unknown,
            "{model:?} has {rows} rows"
        );
    }
}

#[test]
fn capabilities_never_exceed_noise_control() {
    for &model in Model::ALL {
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
    // Sample input: `{ Unknown, AirPods1, ... // comment ... AirPodsMax2 }`, one per line.
    let cpp: Vec<&str> = ENUMS
        .scope("enum class AirPodsModel")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && *line != "{" && !line.starts_with("//"))
        .map(|line| line.trim_end_matches(','))
        .collect();
    let rust: Vec<String> = Model::ALL.iter().copied().map(cpp_name).collect();
    assert_eq!(rust, cpp);
    for (int, &model) in Model::ALL.iter().enumerate() {
        assert_eq!(model.as_int(), i64::try_from(int).unwrap(), "{model:?}");
    }
}

#[test]
fn display_names_match_the_cpp_switch() {
    let body = ENUMS.scope("inline QString modelDisplayName(AirPodsModel model)");
    for &model in Model::ALL {
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
    let rust_without_off: Vec<String> = Model::ALL
        .iter()
        .copied()
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

/// The Pro 3's product as BlueZ prints it, and the byte swapped key the C++ map gives it.
const PRO_3_MODALIAS: &str = "bluetooth:v004Cp2027d0000";
const PRO_3_CPP_KEY: u16 = 0x2720;

#[test]
fn a_modalias_gives_the_cpp_key_byte_swapped() {
    let id = product_of_modalias(PRO_3_MODALIAS);
    assert_eq!(id, Some(PRO_3_CPP_KEY.swap_bytes()));
}

#[test]
fn the_powerbeats_keys_of_the_proposed_cpp_entries_are_beats() {
    let proposed_cpp_keys: [u16; 2] = [0x0B20, 0x1D20];
    for key in proposed_cpp_keys {
        assert!(is_beats(key.swap_bytes()), "{key:#06X}");
        assert!(!is_beats(key), "{key:#06X}");
    }
    assert_eq!(BEATS_PRODUCTS.len(), proposed_cpp_keys.len());
}

#[test]
fn the_pro_3_identifier_is_not_beats() {
    assert_eq!(
        product_of_modalias(PRO_3_MODALIAS).map(is_beats),
        Some(false)
    );
}

#[test]
fn a_modalias_names_a_product_only_in_the_shape_bluez_prints() {
    assert_eq!(
        product_of_modalias("bluetooth:v004Cp200Bd0001"),
        Some(0x200B)
    );
    for text in [
        "",
        "bluetooth:v004Cp",
        "bluetooth:v004Cp2027",
        "bluetooth:v004Cp2027d",
        "bluetooth:v004Cp2027d000",
        "bluetooth:v004Cp2027d00000",
        "bluetooth:v004Cp20270d0000",
        "bluetooth:v0059p2027d0000",
        "usb:v05ACp2027d0000",
        "bluetooth:v004cp2027d0000",
        "Bluetooth:v004Cp2027d0000",
        "bluetooth:v004Cp200bd0001",
        "bluetooth:v004Cp2027D0000",
        "bluetooth:v004Cp2027d000a",
        "bluetooth:v004Cp+027d0000",
        "bluetooth:v004Cp20\u{e9}d0000",
        "bluetooth:v004Cp2\u{e9}7d0000",
        "not a modalias at all",
    ] {
        assert_eq!(product_of_modalias(text), None, "{text}");
    }
}

proptest! {
    #[test]
    fn every_product_and_version_reads_back(id: u16, version: u16) {
        let modalias = format!("{APPLE_MODALIAS_PREFIX}{id:04X}d{version:04X}");
        prop_assert_eq!(product_of_modalias(&modalias), Some(id));
    }

    #[test]
    fn near_misses_and_any_text_read_only_in_the_exact_shape(
        product in "[0-9A-F]{4}|[0-9A-Fa-f+ \u{e9}]{3,5}",
        separator in "[dD]",
        version in "[0-9A-F]{4}|[0-9A-Fa-f+ \u{e9}]{3,5}",
        bare in any::<String>(),
    ) {
        let near_miss = format!("{APPLE_MODALIAS_PREFIX}{product}{separator}{version}");
        for text in [near_miss, bare] {
            if let Some(id) = product_of_modalias(&text) {
                let version = text.strip_prefix(&format!("{APPLE_MODALIAS_PREFIX}{id:04X}d"));
                let printed = |version: &str| {
                    u16::from_str_radix(version, 16).is_ok_and(|number| format!("{number:04X}") == version)
                };
                prop_assert!(version.is_some_and(printed), "{text}");
            }
        }
    }
}
