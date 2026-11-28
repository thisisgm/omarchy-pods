//! Ignored oracle verification harness for `pods-opus` against FFmpeg `libopus` and `ffopus`.

#[cfg(test)]
#[path = "../src/probe/oggfile.rs"]
mod oggfile;
#[cfg(test)]
#[path = "support/oracles.rs"]
mod oracles;
#[cfg(test)]
#[path = "../src/probe/xorshift.rs"]
mod xorshift;

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::{Command, Stdio};

    use pods_opus::{Decoder, OutputRate, SAMPLES_48K, TOC_CELT_WIDEBAND_20MS_MONO};
    use pods_samples::opus::{mark, tone};

    use super::oggfile;
    use super::oracles::opus_oracles_installed;
    use super::xorshift::next_random;

    const LSB_SCALE: f64 = 32768.0;
    const TOLERANCE_LSB: f64 = 1.0;
    const AGREEMENT_OF_PEAK: f64 = 1e-3;

    /// Committed valid 15-90 byte `0xb8` packets spanning VBR 6-12 kbps and CBR 12-36 kbps (including Sol's 15-byte `pkt15`).
    const MULTIBITRATE_PACKETS: &[&[u8]] = &[
        &[
            0xb8, 0x7f, 0x54, 0xce, 0xa7, 0x4d, 0x11, 0xf1, 0x21, 0x86, 0xed, 0x67, 0x4e, 0x14,
            0x34,
        ],
        &[
            0xb8, 0x1d, 0xd3, 0x42, 0x52, 0x8c, 0xcd, 0x14, 0xed, 0x22, 0xa1, 0x42, 0x58, 0x5d,
            0x43, 0x1d, 0x15, 0xe8, 0xd2, 0x28,
        ],
        &[
            0xb8, 0x00, 0x61, 0x49, 0x44, 0x8a, 0x16, 0xcf, 0xc5, 0xfb, 0xd2, 0x4a, 0x6a, 0x33,
            0xaa, 0xb4, 0x4a, 0x36, 0x13, 0xd9, 0x6f, 0x68, 0xbe, 0xdb,
        ],
        &[
            0xb8, 0x31, 0x5c, 0xe9, 0xde, 0x6a, 0xbc, 0x67, 0xa3, 0xe1, 0x36, 0x03, 0xa0, 0x6e,
            0x9f, 0xdd, 0x1e, 0x8e, 0x01, 0xd5, 0x66, 0x85, 0x58, 0xff, 0x9c, 0xe5, 0xd6, 0xa6,
            0xa0, 0xbe,
        ],
        &[
            0xb8, 0xdb, 0x25, 0xc3, 0x94, 0x8d, 0xd8, 0x07, 0x56, 0xda, 0x7a, 0x9f, 0x55, 0x24,
            0x98, 0x74, 0x46, 0xb5, 0xff, 0xeb, 0x77, 0xd2, 0xba, 0x9d, 0x85, 0x29, 0x29, 0xd5,
            0xe2, 0xd6, 0xbd, 0x47, 0xa2, 0x91, 0x06, 0x87, 0x21, 0xe1, 0x03, 0x07,
        ],
        &[
            0xb8, 0x31, 0x5c, 0xe9, 0xde, 0xc3, 0xd6, 0xb9, 0xce, 0xc7, 0x3a, 0xdd, 0x84, 0xb1,
            0xc1, 0xb9, 0x5b, 0x60, 0x30, 0x1e, 0x54, 0x2d, 0xd9, 0x15, 0xa1, 0xd9, 0x94, 0x66,
            0xd9, 0xb7, 0x35, 0xe1, 0x72, 0xd6, 0xa1, 0x2a, 0x42, 0x8c, 0xa6, 0x3d, 0x01, 0xe2,
            0xf4, 0x06, 0x72, 0x55, 0x3f, 0x08, 0x13, 0xf3, 0x8c, 0x3e, 0x68, 0x7e, 0x94, 0xe5,
            0x14, 0x2c, 0x62, 0xbc,
        ],
        &[
            0xb8, 0xc0, 0x7f, 0x1f, 0x56, 0x99, 0xf9, 0xc6, 0x7c, 0x41, 0x4a, 0xbd, 0x30, 0x60,
            0xab, 0x0d, 0x60, 0x3e, 0xd8, 0xeb, 0x30, 0x3e, 0x97, 0x7c, 0x68, 0xa7, 0xa4, 0x02,
            0x72, 0x87, 0xec, 0xb9, 0x91, 0xd0, 0x06, 0xa0, 0x15, 0x42, 0x46, 0x98, 0x06, 0x51,
            0x69, 0x47, 0x3e, 0x5d, 0x81, 0x0f, 0x88, 0x40, 0x4e, 0x69, 0x76, 0x37, 0x76, 0x41,
            0x7c, 0xe3, 0xe3, 0x6b, 0xa5, 0x3d, 0x0c, 0x6e, 0x61, 0x47, 0x28, 0x0f, 0xe9, 0x78,
            0x43, 0x2e, 0x05, 0x8a, 0x2d, 0xb4, 0x33, 0x00, 0x7e, 0x14,
        ],
        &[
            0xb8, 0x9c, 0x4a, 0xc7, 0x24, 0x11, 0x87, 0x81, 0x42, 0x70, 0x66, 0x33, 0xf0, 0x0e,
            0x2a, 0x9d, 0x76, 0xf5, 0x67, 0xf2, 0x2a, 0x79, 0x24, 0xd5, 0x48, 0x0a, 0x38, 0xc3,
            0xe2, 0xb0, 0xb7, 0x2f, 0xb3, 0xa2, 0x7b, 0x52, 0xfe, 0xa9, 0x8e, 0x4e, 0x97, 0xb6,
            0xe2, 0xc2, 0x91, 0xfd, 0x81, 0x4c, 0xe9, 0x1b, 0x96, 0xa5, 0xda, 0x87, 0xc7, 0xb2,
            0x8d, 0xaa, 0xcb, 0xd3, 0x0e, 0x65, 0xc8, 0x59, 0xcc, 0x47, 0x53, 0xf6, 0xa5, 0xf0,
            0x9f, 0x7d, 0x84, 0x48, 0xea, 0xc2, 0xca, 0x3d, 0x7a, 0xe9, 0x1e, 0x7c, 0x1b, 0xa9,
            0x51, 0xa3, 0xb1, 0x6c, 0xda, 0x82,
        ],
        &[
            0xb8, 0x02, 0x16, 0x5d, 0xc7, 0x6a, 0x54, 0xfe, 0xb0, 0x69, 0xea, 0xde, 0x06, 0x58,
            0x79, 0xc0, 0xbb, 0x59, 0x27, 0x47, 0x46, 0x31, 0xb5, 0x85, 0xcd, 0x79, 0xe9, 0x62,
            0x4f, 0x5a, 0x8e, 0xb9, 0x33, 0x83, 0x10, 0x24, 0x9c, 0x0f, 0x70, 0xee, 0xe8, 0x57,
            0xaa, 0x51, 0x4e, 0x93, 0x22, 0x7d, 0x77, 0xda, 0x85, 0xa6, 0x84, 0xb2, 0xa5, 0x43,
            0x1b, 0xbe, 0x98, 0xf9, 0xd8, 0xfe, 0xd4, 0x29, 0xcb, 0x9c, 0x66, 0x9e, 0xdc, 0x5f,
            0x9d, 0x38, 0x5f, 0x4e, 0xa3, 0x81, 0x3b, 0x6c, 0x36, 0xd8, 0x32, 0x51, 0x91, 0xa0,
            0x9e, 0xad, 0x66, 0xbd, 0x7b, 0x61,
        ],
    ];

    fn seeded_frames(seed: u64, count: usize) -> Vec<Vec<u8>> {
        let mut rng = seed;
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            let mut f = Vec::with_capacity(81);
            f.push(TOC_CELT_WIDEBAND_20MS_MONO);
            for _ in 0..80 {
                f.push(next_random(&mut rng).to_le_bytes()[0]);
            }
            let _ = next_random(&mut rng);
            let _ = next_random(&mut rng);
            out.push(f);
        }
        out
    }

    /// Generates Sol's exact 81-byte `pkt19` (`dec_uint` saturation regression case, plain xorshift64).
    fn sol_pkt19() -> Vec<u8> {
        let mut state = 0x5eed_c0de_1234_5678_u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut pkt = Vec::with_capacity(81);
        for idx in 0..20_usize {
            pkt.clear();
            pkt.push(TOC_CELT_WIDEBAND_20MS_MONO);
            for _ in 0..80 {
                pkt.push(next().to_le_bytes()[0]);
            }
            if idx == 19 {
                break;
            }
        }
        pkt
    }

    /// Committed 81-byte packet exercising band-0 cap saturation (`cap = 514`) and excess carry into band 1.
    const BAND0_CAP_CARRY_PKT: &[u8] = &[
        0xb8, 0x3b, 0x3d, 0xb5, 0x66, 0x04, 0x22, 0x58, 0x12, 0x08, 0xe2, 0xa7, 0xa0, 0x8d, 0x32,
        0x32, 0xa8, 0xe7, 0xe2, 0x8f, 0x08, 0x72, 0x22, 0xbe, 0xfb, 0xb6, 0xb5, 0xe2, 0xef, 0x54,
        0xf4, 0xdf, 0xea, 0x17, 0x59, 0x6d, 0x09, 0xef, 0x08, 0xde, 0xd9, 0xe4, 0xed, 0x82, 0xb3,
        0x60, 0xdc, 0xc7, 0xf0, 0xd9, 0x8e, 0xe5, 0xa8, 0x0d, 0xe9, 0xae, 0xdb, 0x86, 0x45, 0x87,
        0xfa, 0x1f, 0x7d, 0xe1, 0xbc, 0x87, 0xac, 0x29, 0xf3, 0x22, 0x5a, 0x30, 0xce, 0xef, 0xa6,
        0x7f, 0x49, 0xbf, 0xae, 0x19, 0xc1,
    ];

    /// Sol's 2-packet 6 kbps ordered pair exercising stateful `oldLogE`/`oldLogE2` transient anti-collapse parity.
    const SOL_R2_PAIR_HEX: &[&str] = &[
        "b87f54cea74d11f12186ed674e1434",
        "b870b1fd520c4eaa170ec74de16e0a",
    ];

    /// Regression frames for sub-2-bit fine energy allocation (`6k_f36`) and split-pair rebalance (`36k_f38`, `36k_f61`).
    const STRESS_REGRESSION_HEX: &[(&str, &str)] = &[
        ("stress_6k_f36", "b87faf6c8fbcf812621c6067b75fc5"),
        (
            "stress_36k_f38",
            "b87fc474dc571f1df96f09792a94193368c71920734aaf37e9a3f2bfff7fd24207ab2cf46b87ec32cf089aa23ecae97f8903ebae3a7b0c65965e00e251807890c9d7361c49843843c72fd16205cf27c3c9b7b049d0b013ea8849",
        ),
        (
            "stress_36k_f61",
            "b872bc41cc5e9f78a27a3a826f4786833e23cd9754884a11a60248abbf92d24d64294a57d44e91e009c82012b0a382b6ce34d487b6a3ff487800f4e7ee2b738ae08d9ddeb4603a914738c000040100a86833f54488fff5998236",
        ),
    ];

    /// Eight continuous 12-frame CBR streams (6..36 kbps = 15..90 bytes, 221 Hz + 1703 Hz amplitude-cycled tones).
    const CONTINUOUS_CBR_STREAMS_HEX: &[(&str, &[&str])] = &[
        (
            "cbr_6k",
            &[
                "b87ef554a6fa2b5f5ff3d58ad1cab7",
                "b87d199cfc20b725572a4789e4a7f6",
                "b8ca3ac5a249635a4472c4fac2db62",
                "b876686fa6182ba5419f81b56c0d1f",
                "b8ca5fc7f40d03ba3f19c8632e0362",
                "b8770e82496b1062fd43251a6fb182",
                "b87fc571b755ea978aee3e44b23407",
                "b80bc127a74d5368ed1fc73e083c0c",
                "b87a222851eeaa89228af3c4891f1e",
                "b87d4f311cbb27a9143dde43164110",
                "b87a272093c0238dd069b50a1179b2",
                "b87c8fcba009894c15e7a79fb34506",
            ],
        ),
        (
            "cbr_8k",
            &[
                "b87ef56b0e19509f6c9f013e437df5757e0576ee",
                "b87d1b9d4031938ec70f3c412daf195ddee98faa",
                "b8ca3b16e7536972bf2769c088c9372b244bcb62",
                "b8766877c1ea5192de4a9585fea022903339ae6f",
                "b8ca5fc7f4117ae81113d1b3bbf99f1842206362",
                "b8758484e08658dd3b826608a2f01d96ac800f1c",
                "b87fc571b755ea96b3bd7b3cef46c4c47fc2e5c8",
                "b80bca6c8c343b238898180646b3eb323acc525c",
                "b87a22285267fbd0333f872276ca5079151c9afe",
                "b87d4f311cbb27a9f6cb5fd08c7eab9b37e8b1b0",
                "b87a272093c01ef782a06a8a49cbf00985b0bdf2",
                "b87c8fcb9f5a0996ce394936916b9bc94daacc80",
            ],
        ),
        (
            "cbr_10k",
            &[
                "b87ef56b0e19509f6ca016909e27a5b53f3b1685e9328697dc",
                "b87d1b9d4031938e1fd9c773725f11cd44bb1fb58eb0663e55",
                "b8ca3b16e75367a582e723605602c0d48a1cc866bc175b9762",
                "b8766877c1ea5193615c95608e5bac5b1cfe3a93699f6cf1ef",
                "b8ca5fc7f411955a2be44627f863d0d0edde60060a13a2c762",
                "b8758484f3c9fa62c0adffed8f2c28a1a6b710e0cf0440001c",
                "b87fc571b755ea96b3c75ff1181a26f3d682c53021c21543c8",
                "b80bca6d9c2d7cbc06e9c93e2e5f99893f351257fbe82ba2f4",
                "b87a22284e199c693718eefa4b84eb019c6ca375cf835b5cfe",
                "b87d4f311cbb27a8b598fcb8460253bd55d32e6de981e67330",
                "b87a272093c01efb43a5a71afc3417a066bfbec441e142f3e4",
                "b87c8fcb9f5a0996e0714eb6599f9bac9b798b0fb64441f201",
            ],
        ),
        (
            "cbr_12k",
            &[
                "b87ef56b0e19509f6ca2b394148fe5b6d07f5c64b17a2c5c5630f02bbf68",
                "b89fc589109c267349f92328cc6368c52dd78ba30a8228a6be4f31f7f159",
                "b89fb56ee70631b4a847f2edaad8742e3d90a79514934ab1ee26d636fb79",
                "b8721dd02241bbdd13cffa2f51861a8f0b50ae786ffad8107c022d47a9d5",
                "b8ca587c85f01353a57cd23d780b3698f1b84e0707ce0029f23e5859e762",
                "b8758484f440b170b62469486b70310b65dcf0418136c0fbe271e206401c",
                "b8ca6d2b80b196fb56c8ee56dac59be481d26f5504f93d7c5a16f82f32e2",
                "b8ec87e48b39ca8565c44fd439afba9b37919887da0277e788659acdc76b",
                "b87af10b4d5d953ac9adfb19926f91d1dbc50622a7faf0f43218f8b05973",
                "b87d4f311cbb27a8cd2b82a23bf3ede42c81a135c1f6041c3677baceaf71",
                "b87a2721c27357b3aac11a46798f5b15bf1b5c2f4d7838f9032c1527bfcc",
                "b8ca569a0df58c0c41b421febe1debad8632f88e1280d05dd3eeee095262",
            ],
        ),
        (
            "cbr_16k",
            &[
                "b87ef56b0e19509f6c3846f156fca34fd63b7f7a8d2079fe25183111edf17add5101739afcdb7de8",
                "b89fc589109c268160f2fa28daf759091ceca8426782b49fe5c7a19f452bea0d9870cadf1db7e159",
                "b89fb56ee70631b143fab863debcd6c8821337776a32c76f4cfac9d1b670b7afa9588339abbbbb79",
                "b8721dd02241bbdd13ad46e6d5531875294b2e585b26c10d8058a818d6047a04ea2e187cd47aa5ab",
                "b8ca587c85f0178bceaae80580e1bd589fb9ea6371e208b3500618fb7279c01b77fc6a251263e762",
                "b8758484f440b170b686e109281b7425eae68579f41348a360162d162c10f40ad36509d040f58838",
                "b8caa230ec105c27df7ed1204c6f8db642536e1266b08a593cde37a8f14e673854819c8157d95ae2",
                "b8f043af8a5178aff3a6dd0e3c6627f561c3ee93f884b027c94284b3e2d2f6bd7867fc23a56d876b",
                "b87c2b47e3e9deabdd662ff418f2f0a1ccfc7134c4fc0f862c5300274188e94b6040daab83458dc0",
                "b87d4f311cbb27a9063d2f220c2677739beca0ea1959bcd80f81458902020f360b43b4698f4ad59a",
                "b87a2721c273577d62fb4dc4f7f6a72bdc25884f5391b162c009b16d345afe28010d89b5f07ddf9c",
                "b8ca57ee09aa09f5a81b9135732182d677829bb29768968b03028bd8634595dc1cb2ef0ff862e262",
            ],
        ),
        (
            "cbr_24k",
            &[
                "b87ef56b0e19509e39263c8e3b3e5af4b213b73190bdbe66a19c712d935bed7ff5a6bdbd1ae30b3397adde9154e57ea102a0448d7ae099b7eda6df29",
                "b89fc5893dd69cb8cb99f6ec1501675ad8dda186b4804965528a5ffc1bd8ccba49fe5c7a033e7d2576eeca751f047d60879a8a101a3314edc37d8259",
                "b89fb56ee70632537f3dd276c10962d0331055d471b2d664ee3af34568679809abf6fd87b9fff87bbeaf545a08be469322b10dabb8cb1b95dbbbe779",
                "b8721dd02241c06b566065b8eff88989ce8fefc82d25b17ff0b394751533cdb02c540c6bf502cc081e23fbad75610ab8ca91053d6dd1978f7552bd67",
                "b8ca588839099bb813cf7b0f4aad47aafb33b1eed284f2aab600f9dda548162d4441cc318b0dfd1e5f07ac6daf00776ebfc0ef28aa500b133a97cf62",
                "b8758484f3cd887a4c4de124e24bdc3534a7e541fa74180001c670b3e7cccb823140c6a5031aacb5ce92000f1388013e320719076154240d019505e0",
                "b8caa230ec1028fd417a369386d8f8f64f55e7d43d3903ad7051f1332ec4508cb26420344e17d4053657be612f7b103fa110697595582059a51276e2",
                "b8f043af8a517908fb9bdb1254b325568d26ab624b46ce36e90444cd2f2d3b12e6d569cd65dbfd913e5f335e3a9dba9526e5bf1d199d62e4d3fb476b",
                "b87c2b4de40cbddf323b7a3b951d2fd8f4c9e6b59e5bdf10805adafdde5568446004f6dd637d93f327d1daf65a4af7a1d040924d56341d45c950c781",
                "b87d4f311cbab419830bb04229876313c421244b434b760696270e69c92a19ff4fb613af588f14a8f2532f8447fd247a2fe42c1925066b552aa73db4",
                "b87a2721c25c8b975638938c660d2f778ae998d747e24959b08d7a05de601929ea92c580134eefcd8844487651cd6e9415dbbef6ad48b76a7d76bd30",
                "b8ca569a0df58b68ac98867784ee3c984b122cd9182058cb96237a5cc5c07777ec40066c0ebfcc26a68a342eb549084d269e9a39a5355f85494a4262",
            ],
        ),
        (
            "cbr_32k",
            &[
                "b87ef56b0e19509e39263c8e3b3e5af4b5a2474f640e0dcfbc429a204f7035397a94af2b781038c7e5199b0674f4fc10314caf468867372e6d8127fcce527739ebe6faa0a744edf1af75a359da6cfa59",
                "b89fc5893dd69cb8cb99f6ec1504752b95224e18b1e59ab3f8014f086e26d12abf185b2a9fd089f6d45321fd31d407d698001a7879f55a499a18a7d6b1ef5d5fafd743df409170d43de28ee70ded8259",
                "b89fb56ee70632537f3dd276c108d928b29c29cc52c590419acbc7807694716a387c2611f592f83e540cdf03eab2f5d210ab31833e77b043ed870b669f38994e952d661386392f8f65a54afabc75e779",
                "b8721dd02241c06b566065b87ebf2eeeba10bdef000a5fd46904694f0a3f6e56295a454f6fd369615421b3796a3d150ac1c883cb3d21b003d461f56f1ded1c20f427c47791689a6bdcb627b5492dec67",
                "b8ca588839099bb7fcb2d60bf69ac19b156de5ebb4b232f9c532ced2abff181a03a620b3ace6ab406d58a8a8133dfab8d4691f6c538a6b2dd387705b91207bffbe95316b7764025bbac4c579d66f1b62",
                "b8758484f3cd887a4c4de0bc0db97a05972ce69c868f864f77138d0adbc4c6b8b80920429b39ed2e2c6d9a00000632b9bc1f73f4c05180800e218a47269d68943d411ffa950fabc987817c82a8a21da0",
                "b8caa230ec102906c0feaba786a320c14ec1f8857800eef711753863c1f7c4e37a8e521cfc8168c0fd71d8f2fea3b0cede34f90241b4689845d9a0fd14ec094134b91cb45b240142012ab0d8a0acd6e2",
                "b8f0442d32e8085d9c1cabe1977514807a8971d38f09c1347b2562b22a1774616890f81bc402c8597a46e8f2db16b1c4dfd31315da81c9b7b2bb589eabee57bfbfc956e08c8da178d4705f66d1ea876b",
                "b87c2b47e3df56854b91854ba3588eb4a050b0bd16300d5cedf1df2d14bf241212443a464e2eccd9a95dbb0007f2d9e0fa7bc4015980111101e254554ae1be8cf80400735a6c2ce1f076cbd3e50c3f09",
                "b87d4f311cbad4ea299134cdbccd493b3a1971ab6ef99d437070499debf078faba30000063ff8a1a126e59bf07f80f4ae9e8a80393fc00fea881e2349ee220c8024f1e354f859234a8ed46d95a9cadd3",
                "b87a272093627daddd18fa3c5d4bdf950e610bae724608cacafc64971fbbf3e313804124fea9fb220d0a12126d1941fdf82d9ee10cafd314cc4cecad90f6c68abb9d1a9bb0120c3355912a1f72d9f331",
                "b8ca57ee09aa18cf0f50b9dd3b9747a6dd300a6735131f9fc737eb1cf8c0f7760ddb4d2ddadda7f56f36c28b1103339507b89181a9a0815377665c7be21af71246464bf796d7681afabfec2a4b478262",
            ],
        ),
        (
            "cbr_36k",
            &[
                "b87ef56b0e19509e39263c8e3b3e5af300cd7fb819b1e98c68520164a43d8956df1f02bb62e29ea50161de923be87916793dd3d3f10a1e23270e1d2a003361be3c9e78753c57f1478716b2681d78ecdfe47b617b0d676959f4b3",
                "b89fc5893dd69cb8cb99f6ec150534006989f9111e044eabd41259b236e2d50bf5224989c406621a5352ed31a8e32c511e16756c327b3ec00fd5f73900ab498b60e5a584072cc37167345a8fbedc1c4b2a95356c3bce15ed8259",
                "b89fb56ee70632537f3dd276c108e6471be14c8feb728bcdc9bc6d7a525edf222233dffd440fa7d50769983a8c6e3e1ea44d4470be3b9cb1d958cf8e4472a5082fcfe2f9685a704f6c071ea1155bb87434eb60ab29f2bc75e779",
                "b8721dd02241c06b566065b872beeb55e85adf9d21bdc236da31db4179b21ede45c96f5354b214539d622ea5fbf8ed99f68c248b32ced37990626004ec003be628ff672444f2fbc9e4f5ffc0e2f8045060e7d9915dad925bd8e7",
                "b8ca588839099bb7fcb2d60bf69ac54637e8af382d171121cc1cec5e944db664b1b0a521d20189df6d131c081a0fbf466e02cc4dcf1ddca23a08351abfdee7b316ca22181e5cbe87a20551e06028ddad99c74c918d79d3379b62",
                "b8758484f3cd887a4c4de0dd7b24f8044b4d15a6450cf1625aa846b47057f50a64cfa06802edd4a9fedc0a0b2000000000000058894de0fb9fa600d7173c000710ade2f0e74477ea2e0c381fd22c832294b2801de24aa2c23689",
                "b8caa230ec102906c0feaba786a320c23c48e16a08da4fe7320678c1a3787f0f9c671d6d089223ea0d61fb855d78b0ae3f551d9e74489afbff74c1c106fa2868fc22fec9688d2435a8b0d095a3a49a6a6437a4b561b140acd6e2",
                "b8f0442d32e807f408146acca1ae04d298840fd067a35a43810a367d9da8f8e8e1573843ba18f9772417958ee706f106fbb18f047fb6efbb1b95a18fa2fac2447d9ea5886cfd4b16ab3c95751d44aa323749dc13ed5686ea876b",
                "b87c2b47e3df56854b75cfbf034767c3418ffe4e4bc58967ae87387cf9e0cb0a375745d302746802e951657855b5d7a000000000005b07cf07d1ef1003612000898a6be23125312f97ccfc0780714334d8e07b04beaf68a8fc13",
                "b87d4f311cbad4ea299134cdbcfb5b858be2840b8737693e98ada93558e1453cd66b862d0c02980d5d440e97aa43f4ad26c8571db067fc7d007d187c000fee23a628061777ea2b79dec6bfe0789941cd8e89b7d7ed9b6a755b93",
                "b87a272093627daddd18fa425aee654a09678992e138c066d685b5adeb822e6cd286a350c0b8d65a8491dd5e9522ba956f5000d43873325f7f0004cd037874a8dd0ff9773a0b143252b7f93df6bf06bfaee273a6a1f5cb5fccf1",
                "b8ca57ee09aa18cf0f50b9dd3b9747a654b51d5bf30254261d609df560092135b8c665ec35c3cab200a6b01da071c910de42fa867d144629745d2728590e9120e7164197b061be0731995459022bd47f12d0cafbb09492238262",
            ],
        ),
    ];

    fn decode_hex(s: &str) -> Vec<u8> {
        s.as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect()
    }

    fn run_ffmpeg(frames: &[&[u8]], decoder_args: &[&str]) -> Vec<f32> {
        let mut ogg = Vec::new();
        oggfile::write(&mut ogg, frames).unwrap();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        fs::write(tmp.path(), &ogg).unwrap();
        let out = Command::new("ffmpeg")
            .args(["-nostdin", "-hide_banner", "-v", "error", "-xerror"])
            .args(decoder_args)
            .arg("-i")
            .arg(tmp.path())
            .args([
                "-map",
                "0:a:0",
                "-c:a",
                "pcm_f32le",
                "-f",
                "f32le",
                "pipe:1",
            ])
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(out.status.success());
        let (chunks, rem) = out.stdout.as_chunks::<4>();
        assert!(
            rem.is_empty(),
            "oracle stdout had {} trailing bytes",
            rem.len()
        );
        chunks.iter().copied().map(f32::from_le_bytes).collect()
    }

    fn frames_over_gate(cand: &[f32], oracle: &[f32], expected_frames: usize) -> usize {
        assert!(!cand.is_empty(), "candidate output was empty");
        assert_eq!(cand.len(), expected_frames.saturating_mul(SAMPLES_48K));
        assert_eq!(cand.len(), oracle.len());
        let (cand_frames, cand_rem) = cand.as_chunks::<SAMPLES_48K>();
        let (oracle_frames, oracle_rem) = oracle.as_chunks::<SAMPLES_48K>();
        assert!(cand_rem.is_empty() && oracle_rem.is_empty());
        let mut over_gate = 0_usize;
        for (cf, of) in cand_frames.iter().zip(oracle_frames.iter()) {
            let mut f_max = 0.0_f64;
            let mut f_peak = 0.0_f64;
            for (&a, &b) in cf.iter().zip(of.iter()) {
                let fa = f64::from(a);
                let fb = f64::from(b);
                assert!(
                    fa.is_finite() && fb.is_finite(),
                    "non-finite sample: cand={fa} oracle={fb}"
                );
                f_max = f_max.max((fa - fb).abs() * LSB_SCALE);
                f_peak = f_peak.max(fa.abs() * LSB_SCALE).max(fb.abs() * LSB_SCALE);
            }
            if f_max > TOLERANCE_LSB.max(AGREEMENT_OF_PEAK * f_peak) {
                over_gate = over_gate.saturating_add(1);
            }
        }
        over_gate
    }

    #[test]
    #[ignore = "requires ffmpeg with libopus and opus decoders"]
    fn pods_opus_matches_libopus_and_ffopus_on_fixtures() {
        if !opus_oracles_installed() {
            return;
        }
        let tone_refs: Vec<&[u8]> = tone().collect();
        let mark_refs: Vec<&[u8]> = mark().collect();
        let seeded = seeded_frames(0x5eed_0018, 200);
        let seeded_refs: Vec<&[u8]> = seeded.iter().map(Vec::as_slice).collect();
        let silent = vec![vec![TOC_CELT_WIDEBAND_20MS_MONO]; 50];
        let silent_refs: Vec<&[u8]> = silent.iter().map(Vec::as_slice).collect();
        let pkt15_refs: [&[u8]; 1] = [MULTIBITRATE_PACKETS[0]];
        let pkt19 = sol_pkt19();
        let pkt19_refs: [&[u8]; 1] = [pkt19.as_slice()];
        let cap_carry_refs: [&[u8]; 1] = [BAND0_CAP_CARRY_PKT];
        let sol_pair: Vec<Vec<u8>> = SOL_R2_PAIR_HEX.iter().map(|h| decode_hex(h)).collect();
        let sol_pair_refs: Vec<&[u8]> = sol_pair.iter().map(Vec::as_slice).collect();
        let stress_vecs: Vec<(&str, Vec<u8>)> = STRESS_REGRESSION_HEX
            .iter()
            .map(|&(name, h)| (name, decode_hex(h)))
            .collect();
        let cbr_streams: Vec<(&str, Vec<Vec<u8>>)> = CONTINUOUS_CBR_STREAMS_HEX
            .iter()
            .map(|&(name, hexes)| (name, hexes.iter().map(|h| decode_hex(h)).collect()))
            .collect();

        let mut suites: Vec<(&str, Vec<&[u8]>)> = vec![
            ("silent", silent_refs),
            ("tone", tone_refs),
            ("mark", mark_refs),
            ("seeded", seeded_refs),
            ("pkt15", pkt15_refs.to_vec()),
            ("multibitrate", MULTIBITRATE_PACKETS.to_vec()),
            ("pkt19", pkt19_refs.to_vec()),
            ("band0_cap_carry", cap_carry_refs.to_vec()),
            ("sol_r2_pair", sol_pair_refs),
        ];
        for (name, pkt) in &stress_vecs {
            suites.push((name, vec![pkt.as_slice()]));
        }
        for (name, pkts) in &cbr_streams {
            suites.push((name, pkts.iter().map(Vec::as_slice).collect()));
        }

        for (name, frames) in &suites {
            let mut dec = Decoder::new(OutputRate::Hz48000);
            let mut cand = Vec::with_capacity(frames.len().saturating_mul(SAMPLES_48K));
            for f in frames {
                cand.extend_from_slice(dec.decode(f).unwrap());
            }
            let lib = run_ffmpeg(frames, &["-c:a", "libopus", "-request_sample_fmt", "flt"]);
            let ff = run_ffmpeg(frames, &["-c:a", "opus", "-request_sample_fmt", "flt"]);
            assert_eq!(
                frames_over_gate(&cand, &lib, frames.len()),
                0,
                "{name} vs libopus"
            );
            assert_eq!(
                frames_over_gate(&cand, &ff, frames.len()),
                0,
                "{name} vs ffopus"
            );
        }
    }
}
