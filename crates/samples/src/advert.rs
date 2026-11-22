//! A proximity advert for tests: a header, one block sealed under the payload key, and an address that resolves under the identity key.

// Layout from `onDeviceDiscovered` in daemon/ble/blemanager.cpp; sealing and address from daemon/ble/bleutils.cpp.

use aes::Aes128;
use aes::cipher::{BlockCipherEncrypt, KeyInit};

/// `data[0]` to `data[10]`, from the type byte `07` to the connection state.
pub const HEADER_LEN: usize = 11;
/// The block `decryptLastBytes` opens, the last 16 bytes.
pub const SEALED_LEN: usize = 16;
/// `proximityPairingBytes`.
pub const ADVERT_LEN: usize = HEADER_LEN + SEALED_LEN;
/// The two top bits of the first written octet of a resolvable private address, `01`.
const ADDRESS_KIND: u8 = 0b1100_0000;
const RESOLVABLE: u8 = 0b0100_0000;

/// The two keys the pods hand over, each in the byte order they arrive in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Keys {
    pub identity: [u8; 16],
    pub payload: [u8; 16],
}

/// An advert as the radio delivers it: the address written `40:00:01:AA:BB:CC` and the data after Apple's company identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Advert {
    pub address: String,
    pub data: [u8; ADVERT_LEN],
}

/// `header`, then `payload` sealed under `keys.payload`, from the address `keys.identity` makes of `random` with its two top bits set to `01`.
#[must_use]
pub fn advert_of(
    header: [u8; HEADER_LEN],
    payload: [u8; SEALED_LEN],
    keys: &Keys,
    random: [u8; 3],
) -> Advert {
    let [r0, r1, r2] = random;
    let r0 = (r0 & !ADDRESS_KIND) | RESOLVABLE;
    let [h0, h1, h2] = hash(&keys.identity, [r2, r1, r0]);
    let octets = [r0, r1, r2, h2, h1, h0];
    let address = octets.map(|octet| format!("{octet:02X}")).join(":");
    let mut data = [0; ADVERT_LEN];
    let (front, back) = data.split_at_mut(HEADER_LEN);
    front.copy_from_slice(&header);
    back.copy_from_slice(&encrypt(&keys.payload, payload));
    Advert { address, data }
}

/// `ah` of `bleutils.cpp`: the low three bytes of `e(k, r')`, `r'` the random part zero padded, all little endian.
fn hash(identity: &[u8; 16], random: [u8; 3]) -> [u8; 3] {
    let mut padded = [0; 16];
    let (front, _) = padded.split_at_mut(random.len());
    front.copy_from_slice(&random);
    let [h0, h1, h2, ..] = aes_big_endian(identity, padded);
    [h0, h1, h2]
}

/// `e` of `bleutils.cpp`: AES-128 with key, data and result turned from little to big endian and back.
fn aes_big_endian(key: &[u8; 16], data: [u8; 16]) -> [u8; 16] {
    let mut key = *key;
    key.reverse();
    let mut data = data;
    data.reverse();
    let mut result = encrypt(&key, data);
    result.reverse();
    result
}

/// One AES-128 block, the inverse of the zero IV single block CBC `decryptLastBytes` opens with.
fn encrypt(key: &[u8; 16], block: [u8; 16]) -> [u8; 16] {
    let mut block = block.into();
    Aes128::new(&(*key).into()).encrypt_block(&mut block);
    block.into()
}

#[cfg(test)]
mod tests {
    use super::{ADVERT_LEN, HEADER_LEN, Keys, SEALED_LEN, advert_of};
    use cpp_source::BLE_MANAGER_SOURCE;

    /// The sample of `onDeviceDiscovered`: `07 19 01 27 20 21 88 8F 11 00 04`.
    const HEADER: [u8; HEADER_LEN] = [
        0x07, 0x19, 0x01, 0x27, 0x20, 0x21, 0x88, 0x8f, 0x11, 0x00, 0x04,
    ];
    /// The identity key of the core specification's `ah` sample, `ec0234a3 57c8ad05 341010a6 0a397d9b`, little endian.
    const SAMPLE_IDENTITY: [u8; 16] = [
        0x9b, 0x7d, 0x39, 0x0a, 0xa6, 0x10, 0x10, 0x34, 0x05, 0xad, 0xc8, 0x57, 0xa3, 0x34, 0x02,
        0xec,
    ];
    /// The sample's random part `708194`, and its hash `0dfbaa`.
    const SAMPLE_RANDOM: [u8; 3] = [0x70, 0x81, 0x94];
    const SAMPLE_ADDRESS: &str = "70:81:94:0D:FB:AA";
    /// The AES-128 example of FIPS-197 appendix C.1: key, plaintext and ciphertext.
    const FIPS_KEY: [u8; 16] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f,
    ];
    const FIPS_PLAIN: [u8; 16] = [
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0xff,
    ];
    const FIPS_SEALED: [u8; 16] = [
        0x69, 0xc4, 0xe0, 0xd8, 0x6a, 0x7b, 0x04, 0x30, 0xd8, 0xcd, 0xb7, 0x80, 0x70, 0xb4, 0xc5,
        0x5a,
    ];

    const KEYS: Keys = Keys {
        identity: SAMPLE_IDENTITY,
        payload: FIPS_KEY,
    };

    #[test]
    fn the_lengths_are_the_cpp_ones() {
        let total = BLE_MANAGER_SOURCE.find(None, "proximityPairingBytes = ", ';');
        assert_eq!(total, format!("{HEADER_LEN} + {SEALED_LEN}"));
        let sealed = BLE_MANAGER_SOURCE.find(None, "data.mid(data.size() - ", ')');
        assert_eq!(sealed, SEALED_LEN.to_string());
    }

    #[test]
    fn the_core_specification_sample_gives_its_address() {
        let advert = advert_of(HEADER, FIPS_PLAIN, &KEYS, SAMPLE_RANDOM);
        assert_eq!(advert.address, SAMPLE_ADDRESS);
    }

    #[test]
    fn the_payload_is_one_block_sealed_under_the_key_as_given() {
        let advert = advert_of(HEADER, FIPS_PLAIN, &KEYS, SAMPLE_RANDOM);
        assert_eq!(advert.data[HEADER_LEN..ADVERT_LEN], FIPS_SEALED);
    }

    #[test]
    fn the_header_leads_the_advert() {
        let advert = advert_of(HEADER, FIPS_PLAIN, &KEYS, SAMPLE_RANDOM);
        assert_eq!(advert.data[..HEADER_LEN], HEADER);
    }

    #[test]
    fn a_random_part_of_another_kind_is_made_resolvable() {
        let [first, second, third] = SAMPLE_RANDOM;
        for kind in [0x00, 0x80, 0xc0] {
            let other_kind = [(first & 0x3f) | kind, second, third];
            let advert = advert_of(HEADER, FIPS_PLAIN, &KEYS, other_kind);
            assert_eq!(advert.address, SAMPLE_ADDRESS);
        }
    }
}
