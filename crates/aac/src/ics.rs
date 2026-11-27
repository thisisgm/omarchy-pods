//! ER AAC-ELD `individual_channel_stream` parsing, inverse quantization and PNS (ISO/IEC 14496-3 Clauses 4.6.2, 4.6.3, 4.6.13).

use crate::Error;
use crate::bits::BitReader;
use crate::huffman::{
    HuffTable, tables as huff_tables, unpack_pair_signed, unpack_pair_unsigned, unpack_quad_signed,
    unpack_quad_unsigned,
};
use crate::tables::{BAND_EDGES, NUM_SWB, inv_quant_mag, math as math_tables};
use crate::tns::TnsData;

const ZERO_HCB: u8 = 0;
const RESERVED_HCB: u8 = 12;
const NOISE_HCB: u8 = 13;
const INTENSITY_HCB2: u8 = 14;
const INTENSITY_HCB: u8 = 15;
const ESC_HCB: u8 = 11;
const SECTION_LEN_ESCAPE: u32 = 31;
/// `hcod_sf` symbol meaning a scalefactor delta of zero (ISO/IEC 14496-3 Clause 4.6.2.3.2).
const SF_DELTA_ZERO: i32 = 60;
/// Noise energy starts at `global_gain - NOISE_OFFSET` (ISO/IEC 14496-3 Clause 4.6.13.3).
const NOISE_OFFSET: i32 = 90;
/// Width of the first noise energy, sent as PCM rather than Huffman coded.
const NOISE_PCM_BITS: u32 = 9;
/// Bias subtracted from the first noise energy PCM value.
const NOISE_PCM_BIAS: i32 = 256;
/// Largest regular scalefactor (`sf` is 8 bits).
const MAX_SCALE_FACTOR: i32 = 255;
/// Noise energy `n` scales by `2^((n - NOISE_GAIN_OFFSET) / 4)`: the spec's `2^(n/4)` times the `2^-15` float output scale.
const NOISE_GAIN_OFFSET: i32 = 60;
/// Noise energy clamp, so the loudest noise band equals the loudest regular band (`255 - 100`).
const NOISE_NRG_MIN: i32 = -100;
const NOISE_NRG_MAX: i32 = 155;
/// Multiplier and increment of the 32-bit linear congruential noise generator.
const LCG_MULTIPLIER: u32 = 1_664_525;
const LCG_INCREMENT: u32 = 1_013_904_223;
/// ISO/IEC 14496-3 Clause 4.6.3.3 caps the escape prefix at 8 ones, so `|q| <= 8191`.
const MAX_ESCAPE_PREFIX_ONES: u32 = 8;

/// Parsed frame ready for TNS and filterbank synthesis (zero heap allocations).
pub(crate) struct ParsedFrame {
    pub(crate) max_sfb: usize,
    pub(crate) tns: TnsData,
}

/// Parses one ER AAC-ELD single-channel element into `spec` without mutating filterbank state; sample `b4 04 42 50` is gain 180, `max_sfb` 1, one CB1 band, delta 0, no TNS, one quad.
pub(crate) fn parse_eld_sce(
    reader: &mut BitReader<'_>,
    spec: &mut [f64; 480],
    pns_seed: &mut u32,
) -> Result<ParsedFrame, Error> {
    let global_gain = i32::try_from(reader.read_bits(8)?)
        .ok()
        .ok_or(Error::InvalidBitstream)?;
    let max_sfb = usize::try_from(reader.read_bits(6)?)
        .ok()
        .ok_or(Error::InvalidBitstream)?;
    if max_sfb > NUM_SWB {
        return Err(Error::MaxSfbOutOfRange(max_sfb));
    }

    let sfb_cb = parse_sections(reader, max_sfb)?;
    let sfb_sf = parse_scale_factors(reader, max_sfb, global_gain, &sfb_cb)?;
    let tns = if reader.read_bool()? {
        TnsData::parse(reader)?
    } else {
        TnsData::default()
    };

    spec.fill(0.0);
    let huff = huff_tables();
    let math = math_tables();
    let mut local_seed = *pns_seed;

    for (sfb, (&cb, &band_sf)) in sfb_cb.iter().zip(sfb_sf.iter()).take(max_sfb).enumerate() {
        let (Some(&start), Some(&end)) = (BAND_EDGES.get(sfb), BAND_EDGES.get(sfb.wrapping_add(1)))
        else {
            return Err(Error::InvalidBitstream);
        };
        let Some(band_spec) = spec.get_mut(usize::from(start)..usize::from(end)) else {
            return Err(Error::InvalidBitstream);
        };
        match cb {
            ZERO_HCB | INTENSITY_HCB2 | INTENSITY_HCB => {}
            NOISE_HCB => fill_pns_band(band_spec, band_sf, &mut local_seed),
            1..=11 => {
                let sf_idx = usize::try_from(band_sf)
                    .ok()
                    .ok_or(Error::ScaleFactorOutOfRange(band_sf))?;
                let &gain = math
                    .sf_gain
                    .get(sf_idx)
                    .ok_or(Error::ScaleFactorOutOfRange(band_sf))?;
                let cb_idx = usize::from(cb.wrapping_sub(1));
                let table = huff.spectral.get(cb_idx).ok_or(Error::ReservedCodebook)?;
                decode_spectral_band(reader, cb, table, gain, band_spec)?;
            }
            _ => return Err(Error::ReservedCodebook),
        }
    }

    *pns_seed = local_seed;
    Ok(ParsedFrame { max_sfb, tns })
}

/// Parses `section_data()` (`sect_len_bits = 5`, Table 4.52); sample `0001 00010` is one CB1 section of 2 bands and `0000 11111 00100` is one zero section of 35.
fn parse_sections(reader: &mut BitReader<'_>, max_sfb: usize) -> Result<[u8; NUM_SWB], Error> {
    let mut sfb_cb = [ZERO_HCB; NUM_SWB];
    let mut band = 0_usize;
    while band < max_sfb {
        let cb = u8::try_from(reader.read_bits(4)?)
            .ok()
            .ok_or(Error::InvalidBitstream)?;
        if cb == RESERVED_HCB {
            return Err(Error::ReservedCodebook);
        }
        let mut sect_len = 0_usize;
        loop {
            let incr = reader.read_bits(5)?;
            let incr_usize = usize::try_from(incr).ok().ok_or(Error::InvalidBitstream)?;
            sect_len = sect_len
                .checked_add(incr_usize)
                .ok_or(Error::InvalidSection)?;
            if sect_len > max_sfb {
                return Err(Error::InvalidSection);
            }
            if incr != SECTION_LEN_ESCAPE {
                break;
            }
        }
        let end = band.checked_add(sect_len).ok_or(Error::InvalidSection)?;
        if sect_len == 0 || end > max_sfb {
            return Err(Error::InvalidSection);
        }
        if let Some(slice) = sfb_cb.get_mut(band..end) {
            slice.fill(cb);
        } else {
            return Err(Error::InvalidSection);
        }
        band = end;
    }
    Ok(sfb_cb)
}

/// Parses `scale_factor_data()` (Table 4.53, Clause 4.6.2.3); sample bit `0` is a zero delta and a first noise band reads 9 PCM bits (`100000000` for zero).
fn parse_scale_factors(
    reader: &mut BitReader<'_>,
    max_sfb: usize,
    global_gain: i32,
    sfb_cb: &[u8; NUM_SWB],
) -> Result<[i32; NUM_SWB], Error> {
    let huff = huff_tables();
    let mut sfb_sf = [0_i32; NUM_SWB];
    let mut sf = global_gain;
    let mut noise_pcm_flag = true;
    let mut noise_nrg = global_gain.wrapping_sub(NOISE_OFFSET);

    for (cb, sf_slot) in sfb_cb.iter().copied().zip(sfb_sf.iter_mut()).take(max_sfb) {
        match cb {
            ZERO_HCB => {}
            NOISE_HCB => {
                if noise_pcm_flag {
                    let delta = i32::try_from(reader.read_bits(NOISE_PCM_BITS)?)
                        .ok()
                        .ok_or(Error::InvalidBitstream)?
                        .wrapping_sub(NOISE_PCM_BIAS);
                    noise_nrg = noise_nrg.wrapping_add(delta);
                    noise_pcm_flag = false;
                } else {
                    let sym = i32::from(huff.sf.decode(reader)?);
                    noise_nrg = noise_nrg.wrapping_add(sym.wrapping_sub(SF_DELTA_ZERO));
                }
                *sf_slot = noise_nrg;
            }
            INTENSITY_HCB2 | INTENSITY_HCB => {
                // Valid in syntax; mono SCE discards intensity-stereo position delta.
                let _delta = huff.sf.decode(reader)?;
            }
            1..=11 => {
                let sym = i32::from(huff.sf.decode(reader)?);
                sf = sf.wrapping_add(sym.wrapping_sub(SF_DELTA_ZERO));
                if !(0..=MAX_SCALE_FACTOR).contains(&sf) {
                    return Err(Error::ScaleFactorOutOfRange(sf));
                }
                *sf_slot = sf;
            }
            _ => return Err(Error::ReservedCodebook),
        }
    }
    Ok(sfb_sf)
}

fn decode_spectral_band(
    reader: &mut BitReader<'_>,
    cb: u8,
    table: &HuffTable,
    gain: f64,
    band: &mut [f64],
) -> Result<(), Error> {
    match cb {
        1 | 2 => {
            for quad_out in band.as_chunks_mut::<4>().0 {
                let sym = table.decode(reader)?;
                let vals = unpack_quad_signed(sym);
                for (dst, q) in quad_out.iter_mut().zip(vals) {
                    if q != 0 {
                        *dst = if q > 0 { gain } else { -gain };
                    }
                }
            }
        }
        3 | 4 => {
            for quad_out in band.as_chunks_mut::<4>().0 {
                let sym = table.decode(reader)?;
                let mags = unpack_quad_unsigned(sym);
                for (dst, mag) in quad_out.iter_mut().zip(mags) {
                    if mag != 0 {
                        let neg = reader.read_bool()?;
                        let scaled = inv_quant_mag(mag) * gain;
                        *dst = if neg { -scaled } else { scaled };
                    }
                }
            }
        }
        5 | 6 => {
            for pair_out in band.as_chunks_mut::<2>().0 {
                let sym = table.decode(reader)?;
                let vals = unpack_pair_signed(sym);
                for (dst, q) in pair_out.iter_mut().zip(vals) {
                    if q != 0 {
                        let scaled = inv_quant_mag(q.unsigned_abs()) * gain;
                        *dst = if q < 0 { -scaled } else { scaled };
                    }
                }
            }
        }
        7..=11 => {
            let base = match cb {
                7 | 8 => 8_u32,
                9 | 10 => 13_u32,
                _ => 17_u32,
            };
            for pair_out in band.as_chunks_mut::<2>().0 {
                let sym = table.decode(reader)?;
                let mut mags = unpack_pair_unsigned(sym, base);
                let mut signs = [false; 2];
                for (mag, sign) in mags.iter().zip(signs.iter_mut()) {
                    if *mag != 0 {
                        *sign = reader.read_bool()?;
                    }
                }
                if cb == ESC_HCB {
                    for mag in &mut mags {
                        if *mag == 16 {
                            *mag = read_escape(reader)?;
                        }
                    }
                }
                for ((dst, mag), neg) in pair_out.iter_mut().zip(mags).zip(signs) {
                    if mag != 0 {
                        let scaled = inv_quant_mag(mag) * gain;
                        *dst = if neg { -scaled } else { scaled };
                    }
                }
            }
        }
        _ => return Err(Error::ReservedCodebook),
    }
    Ok(())
}

/// Reads a CB11 escape sequence (`|q| <= 8191`, Clause 4.6.3.3); sample `0 0101` is 21 and `11111111 0 111111111111` is 8191.
fn read_escape(reader: &mut BitReader<'_>) -> Result<u32, Error> {
    let mut ones = 0_u32;
    while reader.read_bool()? {
        ones = ones.wrapping_add(1);
        if ones > MAX_ESCAPE_PREFIX_ONES {
            return Err(Error::EscapeOverflow);
        }
    }
    let width = ones.wrapping_add(4);
    let low = reader.read_bits(width)?;
    Ok((1_u32 << width) | low)
}

/// Fills one PNS band with energy-normalized pseudo-random noise (ISO/IEC 14496-3 Clause 4.6.13).
fn fill_pns_band(band: &mut [f64], noise_nrg: i32, seed: &mut u32) {
    let mut energy = 0.0_f64;
    for slot in band.iter_mut() {
        *seed = seed
            .wrapping_mul(LCG_MULTIPLIER)
            .wrapping_add(LCG_INCREMENT);
        let signed = i32::from_le_bytes(seed.to_le_bytes());
        let sample = f64::from(signed);
        *slot = sample;
        energy += sample * sample;
    }
    if energy > 0.0 {
        let clamped_nrg = noise_nrg.clamp(NOISE_NRG_MIN, NOISE_NRG_MAX);
        let exponent = f64::from(clamped_nrg.wrapping_sub(NOISE_GAIN_OFFSET)) * 0.25;
        let scale = exponent.exp2() / energy.sqrt();
        for slot in band.iter_mut() {
            *slot *= scale;
        }
    }
}
