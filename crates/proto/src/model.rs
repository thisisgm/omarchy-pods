//! The supported models, their published numbers and what each one can do.

/// A supported model, named as the C++ `AirPodsModel` enum names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Model {
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

/// What a model can do, one flag per status document key.
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

impl Model {
    /// The model a metadata packet's model number names, `Unknown` for a number not in the table.
    // Sample input: `A3064`, matched exactly.
    #[must_use]
    pub fn from_number(number: &str) -> Self {
        MODEL_NUMBERS
            .iter()
            .find(|&&(known, _)| known == number)
            .map_or(Self::Unknown, |&(_, model)| model)
    }

    /// The number the status document publishes as `model_int`; persisted, so never renumbered.
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

    /// The name the status document publishes as `model_name`, empty for `Unknown`.
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

    /// The model's features, by Apple's compare table; `Unknown` keeps the listening modes and gains nothing else.
    #[must_use]
    pub fn capabilities(self) -> Capabilities {
        // Apple's own column set for Adaptive Audio and Conversation Awareness.
        let h2_listening = matches!(
            self,
            Self::AirPods4ANC
                | Self::AirPodsPro2Lightning
                | Self::AirPodsPro2USBC
                | Self::AirPodsPro3
                | Self::AirPodsMax2
        );
        Capabilities {
            // The Pro 3 accepts the Off packet and ignores it.
            noise_off: self != Self::AirPodsPro3,
            noise_control: matches!(
                self,
                Self::Unknown
                    | Self::AirPods4ANC
                    | Self::AirPodsPro
                    | Self::AirPodsPro2Lightning
                    | Self::AirPodsPro2USBC
                    | Self::AirPodsPro3
                    | Self::AirPodsMaxLightning
                    | Self::AirPodsMaxUSBC
                    | Self::AirPodsMax2
            ),
            adaptive: h2_listening,
            conversational_awareness: h2_listening,
            one_bud_anc: matches!(
                self,
                Self::AirPods4ANC
                    | Self::AirPodsPro
                    | Self::AirPodsPro2Lightning
                    | Self::AirPodsPro2USBC
                    | Self::AirPodsPro3
            ),
            is_pro_series: matches!(
                self,
                Self::AirPodsPro
                    | Self::AirPodsPro2Lightning
                    | Self::AirPodsPro2USBC
                    | Self::AirPodsPro3
            ),
            is_headset: matches!(
                self,
                Self::AirPodsMaxLightning | Self::AirPodsMaxUSBC | Self::AirPodsMax2
            ),
        }
    }
}

#[cfg(test)]
mod tests;
