//! Support tests: the advert parser, the address check and the opened block, against protocol fixtures and the core specification's sample.

use super::*;
use crate::battery::BatteryReport;
use crate::model::{Model, Product, product, product_of_modalias};
use pods_samples::advert::{HEADER_LEN, Keys, PRODUCT_AT, address_of, advert_of};
use pods_samples::{IDENTITY_KEY, PAYLOAD_KEY};
use proptest::prelude::*;

/// An example proximity advert header: `07 19 01 27 20 21 88 8F 11 00 04`.
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
fn the_product_reads_little_endian() {
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
fn the_status_maps_ear_bits_to_primary_and_secondary() {
    let both = STATUS_PRIMARY_LEFT | STATUS_THIS_POD_IN_CASE;
    // The status bits no field reads, set together to show they change nothing.
    let unread = !(both | STATUS_EAR_HIGH | STATUS_EAR_LOW);
    // Each row names the side, case placement and bits assigned to the two ears.
    for (status, primary_left, in_case, primary_bit, secondary_bit) in [
        (0, false, false, STATUS_EAR_LOW, STATUS_EAR_HIGH),
        (
            STATUS_PRIMARY_LEFT,
            true,
            false,
            STATUS_EAR_LOW,
            STATUS_EAR_HIGH,
        ),
        (
            STATUS_THIS_POD_IN_CASE,
            false,
            true,
            STATUS_EAR_HIGH,
            STATUS_EAR_LOW,
        ),
        (both, true, true, STATUS_EAR_HIGH, STATUS_EAR_LOW),
    ] {
        for (bits, primary, secondary) in [
            (0, false, false),
            (primary_bit, true, false),
            (secondary_bit, false, true),
            (primary_bit | secondary_bit, true, true),
        ] {
            let byte = status | bits;
            let advert = parse(&with(STATUS_AT, byte)).unwrap();
            let expected = EarReport::from_in_ear(primary, secondary);
            assert_eq!(advert.ear, expected, "{byte:#04x}");
            assert_eq!(advert.primary_left, primary_left, "{byte:#04x}");
            assert_eq!(advert.this_pod_in_case, in_case, "{byte:#04x}");
            let noisy = byte | unread;
            assert_eq!(parse(&with(STATUS_AT, noisy)), Ok(advert), "{noisy:#04x}");
        }
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
