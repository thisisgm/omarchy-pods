//! The supported models, their published numbers and product identifiers, and what each one can do.

/// Declares `Model` and `Model::ALL` from one list of variants, so `ALL` cannot leave one out.
macro_rules! models {
    ($($(#[$attr:meta])* $variant:ident,)+) => {
        /// Named as the C++ `AirPodsModel` enum names those it holds.
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
    AirPods5,
    AirPods5WirelessCase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is a separate capability of the model"
)]
pub struct Capabilities {
    pub noise_off: bool,
    pub noise_control: bool,
    pub adaptive: bool,
    pub conversational_awareness: bool,
    pub one_bud_anc: bool,
    pub is_pro_series: bool,
    pub is_headset: bool,
    /// Known AirPods report wear, including head detection on AirPods Max.
    pub ear_detection: bool,
}

/// Apple's published model numbers (support.apple.com/en-us/109525), sorted by model number.
pub const MODEL_NUMBERS: &[(&str, Model)] = &[
    ("A1523", Model::AirPods1),
    ("A1722", Model::AirPods1),
    ("A2031", Model::AirPods2),
    ("A2032", Model::AirPods2),
    ("A2083", Model::AirPodsPro),
    ("A2084", Model::AirPodsPro),
    ("A2096", Model::AirPodsMaxLightning),
    ("A2564", Model::AirPods3),
    ("A2565", Model::AirPods3),
    ("A2698", Model::AirPodsPro2Lightning),
    ("A2699", Model::AirPodsPro2Lightning),
    ("A2931", Model::AirPodsPro2Lightning),
    ("A3047", Model::AirPodsPro2USBC),
    ("A3048", Model::AirPodsPro2USBC),
    ("A3049", Model::AirPodsPro2USBC),
    ("A3050", Model::AirPods4),
    ("A3053", Model::AirPods4),
    ("A3054", Model::AirPods4),
    ("A3055", Model::AirPods4ANC),
    ("A3056", Model::AirPods4ANC),
    ("A3057", Model::AirPods4ANC),
    ("A3063", Model::AirPodsPro3),
    ("A3064", Model::AirPodsPro3),
    ("A3065", Model::AirPodsPro3),
    ("A3184", Model::AirPodsMaxUSBC),
    ("A3439", Model::AirPods5WirelessCase),
    ("A3440", Model::AirPods5WirelessCase),
    ("A3441", Model::AirPods5WirelessCase),
    ("A3454", Model::AirPodsMax2),
    ("A3531", Model::AirPods5),
    ("A3532", Model::AirPods5),
    ("A3533", Model::AirPods5),
];

/// Returns the first number for the reported model, or the report itself for an unknown model.
#[must_use]
pub fn canonical_model_number(number: &str) -> &str {
    let model = Model::from_number(number);
    MODEL_NUMBERS
        .iter()
        .find(|&&(_, known)| known == model)
        .map_or(number, |&(canonical, _)| canonical)
}

/// What a product identifier names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Product {
    AirPods(Model),
    Beats,
    Unknown,
}

/// Product identifiers as `Modalias` prints them, sorted by Bluetooth product ID.
const AIRPODS_PRODUCTS: &[(u16, Model)] = &[
    (0x2002, Model::AirPods1),
    (0x200A, Model::AirPodsMaxLightning),
    (0x200E, Model::AirPodsPro),
    (0x200F, Model::AirPods2),
    (0x2013, Model::AirPods3),
    (0x2014, Model::AirPodsPro2Lightning),
    (0x2019, Model::AirPods4),
    (0x201B, Model::AirPods4ANC),
    (0x201F, Model::AirPodsMaxUSBC),
    (0x2024, Model::AirPodsPro2USBC),
    (0x2027, Model::AirPodsPro3),
    (0x202D, Model::AirPodsMax2),
    (0x2030, Model::AirPods5WirelessCase),
    (0x2036, Model::AirPods5),
];

/// Beats products under Apple's vendor identifier, as `Modalias` prints them.
const BEATS_PRODUCTS: &[u16] = &[
    0x2003, // Powerbeats 3
    0x2005, // BeatsX
    0x2006, // Beats Solo 3 Wireless
    0x2009, // Beats Studio 3 Wireless
    0x200B, // Powerbeats Pro
    0x200C, // Beats Solo Pro
    0x200D, // Powerbeats (4th generation)
    0x2010, // Beats Flex
    0x2011, // Beats Studio Buds
    0x2012, // Beats Fit Pro
    0x2016, // Beats Studio Buds +
    0x2017, // Beats Studio Pro
    0x201D, // Powerbeats Pro 2
    0x2025, // Beats Solo 4
    0x2026, // Beats Solo Buds
    0x202F, // Powerbeats Fit
    0x2038, // Beats 360
];

/// The Bluetooth SIG vendor source and Apple's company identifier `0x004C`.
const APPLE_MODALIAS_PREFIX: &str = "bluetooth:v004Cp";
const MODALIAS_FIELD_DIGITS: usize = 4;

/// Takes the number `product_of_modalias` gives, never the byte swapped key of the C++ map.
#[must_use]
pub fn product(id: u16) -> Product {
    if let Some(&(_, model)) = AIRPODS_PRODUCTS.iter().find(|&&(known, _)| known == id) {
        Product::AirPods(model)
    } else if BEATS_PRODUCTS.contains(&id) {
        Product::Beats
    } else {
        Product::Unknown
    }
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
            Self::AirPods5 => 13,
            Self::AirPods5WirelessCase => 14,
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
            Self::AirPods5 | Self::AirPods5WirelessCase => "AirPods 5",
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
            ear_detection: true,
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
                ear_detection: true,
                ..Capabilities::default()
            },
            Self::Unknown => Capabilities {
                ear_detection: false,
                ..listening_modes
            },
            Self::AirPodsMaxLightning | Self::AirPodsMaxUSBC => Capabilities {
                is_headset: true,
                ..listening_modes
            },
            Self::AirPodsMax2 => Capabilities {
                is_headset: true,
                ..h2_listening
            },
            Self::AirPods4ANC | Self::AirPods5 | Self::AirPods5WirelessCase => Capabilities {
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
