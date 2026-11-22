//! The supported models, their published numbers and product identifiers, and what each one can do.

/// Declares `Model` and `Model::ALL` from one list of variants, so `ALL` cannot leave one out.
macro_rules! models {
    ($($(#[$attr:meta])* $variant:ident,)+) => {
        /// Named as the C++ `AirPodsModel` enum names it.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
        pub enum Model {
            $($(#[$attr])* $variant,)+
        }

        impl Model {
            /// In `model_int` order.
            #[cfg(test)]
            const ALL: &[Self] = &[$(Self::$variant,)+];
        }
    };
}

models! {
    #[default]
    Unknown,
    AirPods1,
    AirPods2,
    AirPods3,
    AirPodsPro,
    AirPodsPro2Lightning,
    AirPodsPro2USBC,
    AirPodsMaxLightning,
    AirPodsMaxUSBC,
    AirPods4,
    AirPods4ANC,
    AirPodsPro3,
    AirPodsMax2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is one boolean key of the status document"
)]
pub struct Capabilities {
    pub noise_off: bool,
    pub noise_control: bool,
    pub adaptive: bool,
    pub conversational_awareness: bool,
    pub one_bud_anc: bool,
    pub is_pro_series: bool,
    pub is_headset: bool,
}

/// Apple's published model numbers (support.apple.com/en-us/109525), in the order of the C++ map.
const MODEL_NUMBERS: &[(&str, Model)] = &[
    ("A1523", Model::AirPods1),
    ("A1722", Model::AirPods1),
    ("A2032", Model::AirPods2),
    ("A2031", Model::AirPods2),
    ("A2084", Model::AirPodsPro),
    ("A2083", Model::AirPodsPro),
    ("A2096", Model::AirPodsMaxLightning),
    ("A3184", Model::AirPodsMaxUSBC),
    ("A3454", Model::AirPodsMax2),
    ("A2565", Model::AirPods3),
    ("A2564", Model::AirPods3),
    ("A3047", Model::AirPodsPro2USBC),
    ("A3048", Model::AirPodsPro2USBC),
    ("A3049", Model::AirPodsPro2USBC),
    ("A2931", Model::AirPodsPro2Lightning),
    ("A2699", Model::AirPodsPro2Lightning),
    ("A2698", Model::AirPodsPro2Lightning),
    ("A3053", Model::AirPods4),
    ("A3050", Model::AirPods4),
    ("A3054", Model::AirPods4),
    ("A3056", Model::AirPods4ANC),
    ("A3055", Model::AirPods4ANC),
    ("A3057", Model::AirPods4ANC),
    ("A3063", Model::AirPodsPro3),
    ("A3064", Model::AirPodsPro3),
    ("A3065", Model::AirPodsPro3),
];

/// Powerbeats Pro and Powerbeats Pro 2: the keys `0x0B20` and `0x1D20` proposed for the C++ map, byte swapped.
const BEATS_PRODUCTS: &[u16] = &[0x200B, 0x201D];

/// The Bluetooth SIG vendor source and Apple's company identifier `0x004C`.
const APPLE_MODALIAS_PREFIX: &str = "bluetooth:v004Cp";
const MODALIAS_FIELD_DIGITS: usize = 4;

/// Takes the number `product_of_modalias` gives, never the byte swapped key of the C++ map.
#[must_use]
pub fn is_beats(id: u16) -> bool {
    BEATS_PRODUCTS.contains(&id)
}

/// Sample input: `bluetooth:v004Cp2027d0000`, upper case hex as BlueZ prints it; another vendor or shape is `None`.
#[must_use]
pub fn product_of_modalias(modalias: &str) -> Option<u16> {
    let rest = modalias.strip_prefix(APPLE_MODALIAS_PREFIX)?;
    let (product, version) = rest.split_at_checked(MODALIAS_FIELD_DIGITS)?;
    let version = version.strip_prefix('d')?;
    if !is_modalias_field(product) || !is_modalias_field(version) {
        return None;
    }
    u16::from_str_radix(product, 16).ok()
}

fn is_modalias_field(field: &str) -> bool {
    field.len() == MODALIAS_FIELD_DIGITS
        && field
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'A'..=b'F').contains(&byte))
}

impl Model {
    /// Sample input: `A3064`, matched exactly.
    #[must_use]
    pub fn from_number(number: &str) -> Self {
        MODEL_NUMBERS
            .iter()
            .find(|&&(known, _)| known == number)
            .map_or(Self::Unknown, |&(_, model)| model)
    }

    /// Published as `model_int` and persisted, so never renumbered.
    #[must_use]
    pub fn as_int(self) -> i64 {
        match self {
            Self::Unknown => 0,
            Self::AirPods1 => 1,
            Self::AirPods2 => 2,
            Self::AirPods3 => 3,
            Self::AirPodsPro => 4,
            Self::AirPodsPro2Lightning => 5,
            Self::AirPodsPro2USBC => 6,
            Self::AirPodsMaxLightning => 7,
            Self::AirPodsMaxUSBC => 8,
            Self::AirPods4 => 9,
            Self::AirPods4ANC => 10,
            Self::AirPodsPro3 => 11,
            Self::AirPodsMax2 => 12,
        }
    }

    /// Published as `model_name`.
    #[must_use]
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Unknown => "",
            Self::AirPods1 => "AirPods",
            Self::AirPods2 => "AirPods (2nd generation)",
            Self::AirPods3 => "AirPods (3rd generation)",
            // The same name as the plain AirPods 4: the capability keys say which one it is.
            Self::AirPods4 | Self::AirPods4ANC => "AirPods 4",
            Self::AirPodsPro => "AirPods Pro",
            Self::AirPodsPro2Lightning => "AirPods Pro 2",
            Self::AirPodsPro2USBC => "AirPods Pro 2 (USB-C)",
            Self::AirPodsMaxLightning => "AirPods Max",
            Self::AirPodsMaxUSBC => "AirPods Max (USB-C)",
            Self::AirPodsMax2 => "AirPods Max 2",
            Self::AirPodsPro3 => "AirPods Pro 3",
        }
    }

    /// By Apple's compare table; `Unknown` keeps the listening modes and gains nothing else.
    #[must_use]
    pub fn capabilities(self) -> Capabilities {
        let listening_modes = Capabilities {
            noise_off: true,
            noise_control: true,
            ..Capabilities::default()
        };
        // Apple's own column set for Adaptive Audio and Conversation Awareness.
        let h2_listening = Capabilities {
            adaptive: true,
            conversational_awareness: true,
            ..listening_modes
        };
        match self {
            Self::AirPods1 | Self::AirPods2 | Self::AirPods3 | Self::AirPods4 => Capabilities {
                noise_off: true,
                ..Capabilities::default()
            },
            Self::Unknown => listening_modes,
            Self::AirPodsMaxLightning | Self::AirPodsMaxUSBC => Capabilities {
                is_headset: true,
                ..listening_modes
            },
            Self::AirPodsMax2 => Capabilities {
                is_headset: true,
                ..h2_listening
            },
            Self::AirPods4ANC => Capabilities {
                one_bud_anc: true,
                ..h2_listening
            },
            Self::AirPodsPro => Capabilities {
                one_bud_anc: true,
                is_pro_series: true,
                ..listening_modes
            },
            Self::AirPodsPro2Lightning | Self::AirPodsPro2USBC => Capabilities {
                one_bud_anc: true,
                is_pro_series: true,
                ..h2_listening
            },
            // The Pro 3 accepts the Off packet and ignores it.
            Self::AirPodsPro3 => Capabilities {
                noise_off: false,
                one_bud_anc: true,
                is_pro_series: true,
                ..h2_listening
            },
        }
    }
}

#[cfg(test)]
mod tests;
