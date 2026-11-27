//! Model lookup, status values and supported listening features.

use super::*;
use proptest::prelude::*;

/// A row the Rust table holds and the C++ sources do not, named by why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BeyondCpp {
    ReleasedAfter(Model),
    IdPublishedAfter(u16),
    BeatsKeptOut(u16),
}
use BeyondCpp::{BeatsKeptOut, IdPublishedAfter, ReleasedAfter};

/// The C++ comparisons leave out the model and identifier rows, and the Beats rows pin the Beats list, which C++ lacks.
const BEYOND_CPP: &[BeyondCpp] = &[
    ReleasedAfter(Model::AirPods5),
    ReleasedAfter(Model::AirPods5WirelessCase),
    IdPublishedAfter(0x2036),
    IdPublishedAfter(0x2030),
    IdPublishedAfter(0x202D),
    BeatsKeptOut(0x2003),
    BeatsKeptOut(0x2005),
    BeatsKeptOut(0x2006),
    BeatsKeptOut(0x2009),
    BeatsKeptOut(0x200B),
    BeatsKeptOut(0x200C),
    BeatsKeptOut(0x200D),
    BeatsKeptOut(0x2010),
    BeatsKeptOut(0x2011),
    BeatsKeptOut(0x2012),
    BeatsKeptOut(0x2016),
    BeatsKeptOut(0x2017),
    BeatsKeptOut(0x201D),
    BeatsKeptOut(0x2025),
    BeatsKeptOut(0x2026),
    BeatsKeptOut(0x202F),
    BeatsKeptOut(0x2038),
];

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
        ("A3531", Model::AirPods5),
        ("A3532", Model::AirPods5),
        ("A3533", Model::AirPods5),
        ("A3439", Model::AirPods5WirelessCase),
        ("A3440", Model::AirPods5WirelessCase),
        ("A3441", Model::AirPods5WirelessCase),
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
    assert_eq!(Model::AirPods5.display_name(), "AirPods 5");
    assert_eq!(Model::AirPods5WirelessCase.display_name(), "AirPods 5");
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
        (Model::AirPods5, 13),
        (Model::AirPods5WirelessCase, 14),
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
        Model::AirPods5,
        Model::AirPods5WirelessCase,
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
        Model::AirPods5,
        Model::AirPods5WirelessCase,
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
        (Model::AirPods5, true, true, true, true),
        (Model::AirPods5WirelessCase, true, true, true, true),
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
fn model_numbers_in_status_follow_the_public_order() {
    // The status line publishes these numbers, so a reordered variant list must go red here.
    let public = [
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
        (Model::AirPods5, 13),
        (Model::AirPods5WirelessCase, 14),
    ];
    assert_eq!(Model::ALL.len(), public.len());
    for (model, int) in public {
        assert_eq!(model.as_int(), int, "{model:?}");
    }
}

/// Display name and capability flags per model, in the field order of `Capabilities`.
#[test]
fn names_and_capabilities_are_pinned_for_every_model() {
    const T: bool = true;
    const F: bool = false;
    let table: [(Model, &str, [bool; 7]); 15] = [
        (Model::Unknown, "", [T, T, F, F, F, F, F]),
        (Model::AirPods1, "AirPods", [T, F, F, F, F, F, F]),
        (
            Model::AirPods2,
            "AirPods (2nd generation)",
            [T, F, F, F, F, F, F],
        ),
        (
            Model::AirPods3,
            "AirPods (3rd generation)",
            [T, F, F, F, F, F, F],
        ),
        (Model::AirPodsPro, "AirPods Pro", [T, T, F, F, T, T, F]),
        (
            Model::AirPodsPro2Lightning,
            "AirPods Pro 2",
            [T, T, T, T, T, T, F],
        ),
        (
            Model::AirPodsPro2USBC,
            "AirPods Pro 2 (USB-C)",
            [T, T, T, T, T, T, F],
        ),
        (
            Model::AirPodsMaxLightning,
            "AirPods Max",
            [T, T, F, F, F, F, T],
        ),
        (
            Model::AirPodsMaxUSBC,
            "AirPods Max (USB-C)",
            [T, T, F, F, F, F, T],
        ),
        (Model::AirPods4, "AirPods 4", [T, F, F, F, F, F, F]),
        (Model::AirPods4ANC, "AirPods 4", [T, T, T, T, T, F, F]),
        (Model::AirPodsPro3, "AirPods Pro 3", [F, T, T, T, T, T, F]),
        (Model::AirPodsMax2, "AirPods Max 2", [T, T, T, T, F, F, T]),
        (Model::AirPods5, "AirPods 5", [T, T, T, T, T, F, F]),
        (
            Model::AirPods5WirelessCase,
            "AirPods 5",
            [T, T, T, T, T, F, F],
        ),
    ];
    assert_eq!(Model::ALL.len(), table.len());
    assert_eq!(Model::default(), Model::Unknown);
    for (model, name, flags) in table {
        assert_eq!(model.display_name(), name, "{model:?}");
        let c = model.capabilities();
        let actual = [
            c.noise_off,
            c.noise_control,
            c.adaptive,
            c.conversational_awareness,
            c.one_bud_anc,
            c.is_pro_series,
            c.is_headset,
        ];
        assert_eq!(actual, flags, "{model:?}");
    }
}

/// The Pro 3's product as BlueZ prints it, and the byte swapped key the C++ map gives it.
const PRO_3_MODALIAS: &str = "bluetooth:v004Cp2027d0000";
const PRO_3_CPP_KEY: u16 = 0x2720;

#[test]
fn the_lookup_takes_the_modalias_number_not_the_cpp_key() {
    let id = product_of_modalias(PRO_3_MODALIAS);
    assert_eq!(id, Some(PRO_3_CPP_KEY.swap_bytes()));
    assert_eq!(id.map(product), Some(Product::AirPods(Model::AirPodsPro3)));
    assert_eq!(product(PRO_3_CPP_KEY), Product::Unknown);
}

#[test]
fn the_beats_list_is_the_beats_kept_out() {
    let kept_out: Vec<u16> = BEYOND_CPP
        .iter()
        .filter_map(|&row| match row {
            BeatsKeptOut(id) => Some(id),
            ReleasedAfter(_) | IdPublishedAfter(_) => None,
        })
        .collect();
    assert_eq!(BEATS_PRODUCTS, kept_out);
    for &id in BEATS_PRODUCTS {
        assert_eq!(product(id), Product::Beats, "{id:#06X}");
    }
}

#[test]
fn every_identifier_published_after_the_cpp_map_names_airpods() {
    for &row in BEYOND_CPP {
        if let IdPublishedAfter(id) = row {
            assert!(
                matches!(product(id), Product::AirPods(model) if model != Model::Unknown),
                "{id:#06X}"
            );
        }
    }
}

#[test]
fn every_identifier_is_listed_once_and_never_byte_swapped() {
    let ids: Vec<u16> = AIRPODS_PRODUCTS
        .iter()
        .map(|&(id, _)| id)
        .chain(BEATS_PRODUCTS.iter().copied())
        .collect();
    for &(id, model) in AIRPODS_PRODUCTS {
        assert_eq!(product(id), Product::AirPods(model), "{id:#06X}");
    }
    for &id in &ids {
        let listed = ids.iter().filter(|&&other| other == id).count();
        assert_eq!(listed, 1, "{id:#06X}");
        assert_eq!(
            product(id.swap_bytes()),
            Product::Unknown,
            "{id:#06X} byte swapped"
        );
    }
}

#[test]
fn no_identifier_is_both_airpods_and_beats() {
    for &(id, _) in AIRPODS_PRODUCTS {
        assert!(!BEATS_PRODUCTS.contains(&id), "{id:#06X}");
    }
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
