//! Support tests: the advert parser, the address check and the opened block, against the C++ sources and the core specification's sample.

use super::*;
use crate::battery::BatteryReport;
use crate::model::{Model, Product, product, product_of_modalias};
use cpp_source::{BLE_MANAGER_SOURCE, BLE_UTILS};
use pods_samples::advert::{HEADER_LEN, Keys, PRODUCT_AT, address_of, advert_of};
use pods_samples::{IDENTITY_KEY, PAYLOAD_KEY};
use proptest::prelude::*;

/// The sample of `onDeviceDiscovered`: `07 19 01 27 20 21 88 8F 11 00 04`.
const HEADER: [u8; HEADER_LEN] = [
    0x07, 0x19, 0x01, 0x27, 0x20, 0x21, 0x88, 0x8f, 0x11, 0x00, 0x04,
];
const STATUS_AT: usize = 5;
const CASE_AT: usize = 7;
const LID_AT: usize = 8;
/// The identity key of the core specification's `ah` sample, `ec0234a3 57c8ad05 341010a6 0a397d9b`, in the order the pods hand it over.
const SAMPLE_IDENTITY: [u8; 16] = [
    0x9b, 0x7d, 0x39, 0x0a, 0xa6, 0x10, 0x10, 0x34, 0x05, 0xad, 0xc8, 0x57, 0xa3, 0x34, 0x02, 0xec,
];
/// The sample's random part `708194` and its hash `0dfbaa`.
const SAMPLE_ADDRESS: &str = "70:81:94:0D:FB:AA";
/// Primary 80 charging, secondary 60, case 40: `00 D0 3C 28` and twelve synthetic bytes.
const PAYLOAD: [u8; 16] = [
    0x00, 0xd0, 0x3c, 0x28, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12,
];
const RANDOM: [u8; 3] = [0x40, 0x00, 0x01];

fn keys() -> Keys {
    Keys {
        identity: IDENTITY_KEY,
        payload: PAYLOAD_KEY,
    }
}

fn sample() -> [u8; ADVERT_LEN] {
    advert_of(HEADER, PAYLOAD, &keys(), RANDOM).data
}

fn with(at: usize, value: u8) -> [u8; ADVERT_LEN] {
    let mut data = sample();
    data[at] = value;
    data
}

fn address(text: &str) -> BdAddr {
    text.parse().unwrap()
}

/// `onDeviceDiscovered` transcribed: the in ear bits by side, flipped when the right bud is primary, then named primary and secondary.
fn cpp_in_ear(status: u8) -> (bool, bool) {
    let primary_left = status & STATUS_PRIMARY_LEFT != 0;
    let this_in_case = status & STATUS_THIS_POD_IN_CASE != 0;
    let xor_factor = !primary_left ^ this_in_case;
    let left = if xor_factor {
        status & STATUS_EAR_HIGH != 0
    } else {
        status & STATUS_EAR_LOW != 0
    };
    let right = if xor_factor {
        status & STATUS_EAR_LOW != 0
    } else {
        status & STATUS_EAR_HIGH != 0
    };
    if primary_left {
        (left, right)
    } else {
        (right, left)
    }
}

#[test]
fn the_sample_advert_parses_to_its_fields() {
    let data = sample();
    let advert = parse(&data).unwrap();
    assert_eq!(
        advert,
        Advert {
            product: 0x2027,
            primary_left: true,
            this_pod_in_case: false,
            ear: EarReport::from_in_ear(false, false),
            case: None,
            lid: Lid::Unknown,
            connection: ConnectionState::Idle,
            sealed: data[HEADER_LEN..].try_into().unwrap(),
        }
    );
}

#[test]
fn the_product_reads_little_endian_where_the_cpp_key_is_swapped() {
    for (word, modalias, model) in [
        (
            [0x27, 0x20],
            "bluetooth:v004Cp2027d0000",
            Model::AirPodsPro3,
        ),
        ([0x36, 0x20], "bluetooth:v004Cp2036d0000", Model::AirPods5),
        (
            [0x30, 0x20],
            "bluetooth:v004Cp2030d0000",
            Model::AirPods5WirelessCase,
        ),
        (
            [0x2d, 0x20],
            "bluetooth:v004Cp202Dd0000",
            Model::AirPodsMax2,
        ),
    ] {
        let mut data = sample();
        data[PRODUCT_AT..PRODUCT_AT + word.len()].copy_from_slice(&word);
        let advert = parse(&data).unwrap();
        assert_eq!(
            product(advert.product),
            Product::AirPods(model),
            "{word:02x?}"
        );
        assert_eq!(
            product_of_modalias(modalias),
            Some(advert.product),
            "{modalias}"
        );
    }
    let cpp = BLE_MANAGER_SOURCE.find(None, "deviceInfo.modelName = getModelName(", ';');
    assert_eq!(
        cpp,
        "static_cast<quint16>(data[4]) | (static_cast<quint8>(data[3]) << 8))"
    );
}

#[test]
fn a_short_or_wrong_type_advert_is_refused_and_a_long_one_read_by_its_first_bytes() {
    let data = sample();
    for len in 0..ADVERT_LEN {
        let refused = parse(&data[..len]);
        let too_short = ParseError::TooShort {
            parser: Parser::Proximity,
            need: ADVERT_LEN,
            got: len,
        };
        assert_eq!(refused, Err(too_short), "{len}");
    }
    let wrong_type = ParseError::UnknownValue {
        parser: Parser::Proximity,
        field: "type",
        value: 0x10,
    };
    assert_eq!(parse(&with(0, 0x10)), Err(wrong_type));
    let longer = [&data[..], &[0xee; 4]].concat();
    assert_eq!(parse(&longer), parse(&data));
    assert!(parse(&data).is_ok());
}

#[test]
fn an_advert_in_pairing_mode_is_refused() {
    let pairing = ParseError::UnknownValue {
        parser: Parser::Proximity,
        field: "pairing state",
        value: 0x00,
    };
    assert_eq!(parse(&with(2, 0x00)), Err(pairing));
}

#[test]
fn the_cpp_reads_the_same_bytes_and_bits() {
    let line = |start: &str| BLE_MANAGER_SOURCE.find(None, start, ';');
    assert_eq!(
        line("static constexpr int proximityPairingBytes = "),
        format!("{HEADER_LEN} + {SEALED_LEN}")
    );
    assert_eq!(ADVERT_LEN, HEADER_LEN + SEALED_LEN);
    let read_at = |at: usize| format!("static_cast<quint8>(data[{at}])");
    assert_eq!(line("quint8 status = "), read_at(STATUS_AT));
    assert_eq!(line("quint8 flagsAndCaseBattery = "), read_at(CASE_AT));
    assert_eq!(line("quint8 lidIndicator = "), read_at(LID_AT));
    let front = BLE_MANAGER_SOURCE.find(None, "if (data.size() >= ", ')');
    assert_eq!(
        front,
        format!("proximityPairingBytes && data[0] == {PROXIMITY_TYPE:#04x}")
    );
    let pairing = BLE_MANAGER_SOURCE.find(None, "if (data[2] == ", ')');
    assert_eq!(pairing, format!("{PAIRING_MODE:#04x}"));
    let bit = |start: &str| BLE_MANAGER_SOURCE.find(None, start, ')');
    assert_eq!(
        bit("bool primaryLeft = (status & "),
        format!("{STATUS_PRIMARY_LEFT:#04x}")
    );
    assert_eq!(
        bit("deviceInfo.isThisPodInTheCase = (status & "),
        format!("{STATUS_THIS_POD_IN_CASE:#04x}")
    );
    let tenths = BLE_MANAGER_SOURCE.find(None, "int caseNibble = flagsAndCaseBattery & ", ';');
    assert_eq!(tenths, format!("{CASE_TENTHS:#04X}"));
    assert_eq!(
        bit("deviceInfo.caseCharging = (flags & "),
        format!("{:#04x}", CASE_CHARGING >> 4)
    );
    assert_eq!(
        bit("static_cast<quint8>((lidIndicator >> "),
        LID_CLOSED.trailing_zeros().to_string()
    );
}

#[test]
fn cpp_in_ear_is_the_cpp_rule_as_written() {
    let line = |start: &str| BLE_MANAGER_SOURCE.find(None, start, ';');
    assert_eq!(line("bool areValuesFlipped = "), "!primaryLeft");
    assert_eq!(
        line("bool xorFactor = "),
        "areValuesFlipped ^ deviceInfo.isThisPodInTheCase"
    );
    let side = |first: u8, second: u8| {
        format!("xorFactor ? (status & {first:#04x}) != 0 : (status & {second:#04x}) != 0")
    };
    assert_eq!(
        line("deviceInfo.isLeftPodInEar = "),
        side(STATUS_EAR_HIGH, STATUS_EAR_LOW)
    );
    assert_eq!(
        line("deviceInfo.isRightPodInEar = "),
        side(STATUS_EAR_LOW, STATUS_EAR_HIGH)
    );
    assert_eq!(
        line("deviceInfo.isPrimaryInEar = "),
        "primaryLeft ? deviceInfo.isLeftPodInEar : deviceInfo.isRightPodInEar"
    );
}

/// Worked by hand from the C++ lines: the primary pod reads the high bit while it is in the case and the low bit out of it.
#[test]
fn cpp_in_ear_gives_the_primary_the_high_bit_only_in_the_case() {
    let in_case = [
        (STATUS_THIS_POD_IN_CASE, STATUS_EAR_HIGH, STATUS_EAR_LOW),
        (0, STATUS_EAR_LOW, STATUS_EAR_HIGH),
    ];
    for side in [STATUS_PRIMARY_LEFT, 0] {
        for (case_bit, primary_bit, secondary_bit) in in_case {
            let status = side | case_bit;
            assert_eq!(
                cpp_in_ear(status | primary_bit),
                (true, false),
                "{status:#04x}"
            );
            assert_eq!(
                cpp_in_ear(status | secondary_bit),
                (false, true),
                "{status:#04x}"
            );
        }
    }
}

#[test]
fn the_ear_bits_follow_the_cpp_rule_for_every_status() {
    for status in 0..=u8::MAX {
        let (primary, secondary) = cpp_in_ear(status);
        let advert = parse(&with(STATUS_AT, status)).unwrap();
        assert_eq!(
            advert.ear,
            EarReport::from_in_ear(primary, secondary),
            "{status:#04x}"
        );
        assert_eq!(advert.primary_left, status & 0x20 != 0, "{status:#04x}");
    }
}

#[test]
fn the_case_reads_its_tenths_up_to_full_and_its_charging_flag() {
    for byte in 0..=u8::MAX {
        let tenths = byte & 0x0f;
        let wanted = (tenths <= 10).then(|| Charge {
            level: tenths * 10,
            charging: byte & 0x40 != 0,
        });
        assert_eq!(
            parse(&with(CASE_AT, byte)).unwrap().case,
            wanted,
            "{byte:#04x}"
        );
    }
}

#[test]
fn the_lid_is_read_only_from_a_pod_in_the_case() {
    let in_case = STATUS_THIS_POD_IN_CASE;
    let lid = |status: u8, byte: u8| {
        let mut data = with(STATUS_AT, status);
        data[LID_AT] = byte;
        parse(&data).unwrap().lid
    };
    assert_eq!(lid(in_case, 0x00), Lid::Open);
    assert_eq!(lid(in_case, 0x08), Lid::Closed);
    assert_eq!(lid(0x00, 0x08), Lid::Unknown);
    assert_eq!(lid(0x00, 0x00), Lid::Unknown);
}

#[test]
fn the_core_specification_sample_resolves() {
    let identity = Key::from_bytes(SAMPLE_IDENTITY);
    assert!(resolves(address(SAMPLE_ADDRESS), &identity));
    let advert = advert_of(HEADER, PAYLOAD, &keys(), RANDOM);
    assert!(resolves(
        address(&advert.address),
        &Key::from_bytes(IDENTITY_KEY)
    ));
}

#[test]
fn the_cpp_takes_the_hash_and_random_part_where_resolves_does() {
    let verify = BLE_UTILS.body("bool verifyRPA(");
    assert!(
        verify.contains("QByteArray prand = rpa.mid(3, 3);"),
        "{verify}"
    );
    assert!(
        verify.contains("QByteArray hash = rpa.left(3);"),
        "{verify}"
    );
}

#[test]
fn another_identity_key_does_not_resolve() {
    let advert = advert_of(HEADER, PAYLOAD, &keys(), RANDOM);
    assert!(!resolves(
        address(&advert.address),
        &Key::from_bytes(PAYLOAD_KEY)
    ));
    let mut hash_off = address(&advert.address).octets();
    hash_off[5] ^= 0x01;
    assert!(!resolves(
        BdAddr::from(hash_off),
        &Key::from_bytes(IDENTITY_KEY)
    ));
}

#[test]
fn a_static_or_non_resolvable_address_never_resolves() {
    let [_, r1, r2] = RANDOM;
    for kind in [0x00, 0x80, 0xc0] {
        let made = address_of(&IDENTITY_KEY, [kind | 0x01, r1, r2]);
        assert!(
            !resolves(address(&made), &Key::from_bytes(IDENTITY_KEY)),
            "{made}"
        );
    }
}

#[test]
fn open_inverts_a_block_sealed_with_the_same_key() {
    let advert = parse(&sample()).unwrap();
    assert_eq!(open(&advert.sealed, &Key::from_bytes(PAYLOAD_KEY)), PAYLOAD);
    assert_ne!(
        open(&advert.sealed, &Key::from_bytes(IDENTITY_KEY)),
        PAYLOAD
    );
    let cpp = BLE_UTILS.body("QByteArray aes128CbcDecryptZeroIv(");
    assert!(cpp.contains("unsigned char iv[16] = {0};"), "{cpp}");
    assert!(cpp.contains("EVP_aes_128_cbc()"), "{cpp}");
}

#[test]
fn an_opened_block_feeds_parse_decrypted() {
    let advert = parse(&sample()).unwrap();
    let opened = open(&advert.sealed, &Key::from_bytes(PAYLOAD_KEY));
    let report = BatteryReport::parse_decrypted(
        &opened,
        advert.primary_left,
        advert.this_pod_in_case,
        false,
    )
    .unwrap();
    let charge = |level, charging| Some(Charge { level, charging });
    assert_eq!(report.batteries.left, charge(80, true));
    assert_eq!(report.batteries.right, charge(60, false));
    assert_eq!(report.batteries.case, charge(40, false));
}

proptest! {
    #[test]
    fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..64)) {
        drop(parse(&bytes));
        drop(parse(&[&[PROXIMITY_TYPE, 0x19, 0x01][..], &bytes].concat()));
    }

    #[test]
    fn any_address_and_key_never_panic(octets in any::<[u8; 6]>(), key in any::<[u8; 16]>()) {
        let key = Key::from_bytes(key);
        std::hint::black_box(resolves(BdAddr::from(octets), &key));
        std::hint::black_box(open(&[0; 16], &key));
    }
}
