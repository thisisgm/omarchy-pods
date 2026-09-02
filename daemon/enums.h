#pragma once

#include <QMetaType>
#include <QHash>

namespace AirpodsTrayApp
{
    namespace Enums
    {
        Q_NAMESPACE

        enum class NoiseControlMode : quint8
        {
            Off = 0,
            NoiseCancellation = 1,
            Transparency = 2,
            Adaptive = 3,

            MinValue = Off,
            MaxValue = Adaptive,
        };
        Q_ENUM_NS(NoiseControlMode)

        enum class AirPodsModel
        {
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
            // Appended, never inserted: this int is persisted and published as model_int.
            AirPodsMax2,
            PowerbeatsPro
        };
        Q_ENUM_NS(AirPodsModel)

        // Get model enum from model number
        inline AirPodsModel parseModelNumber(const QString &modelNumber)
        {
            // Model numbers taken from https://support.apple.com/en-us/109525
            QHash<QString, AirPodsModel> modelNumberMap = {
                {"A1523", AirPodsModel::AirPods1},
                {"A1722", AirPodsModel::AirPods1},
                {"A2032", AirPodsModel::AirPods2},
                {"A2031", AirPodsModel::AirPods2},
                {"A2084", AirPodsModel::AirPodsPro},
                {"A2083", AirPodsModel::AirPodsPro},
                {"A2096", AirPodsModel::AirPodsMaxLightning},
                {"A3184", AirPodsModel::AirPodsMaxUSBC},
                // A3454 is Apple's code for the 2026 Max 2, read off FCC ID BCG-A3454; its BLE model id is unknown here, so only AAP metadata names it.
                {"A3454", AirPodsModel::AirPodsMax2},
                {"A2565", AirPodsModel::AirPods3},
                {"A2564", AirPodsModel::AirPods3},
                {"A3047", AirPodsModel::AirPodsPro2USBC},
                {"A3048", AirPodsModel::AirPodsPro2USBC},
                {"A3049", AirPodsModel::AirPodsPro2USBC},
                {"A2931", AirPodsModel::AirPodsPro2Lightning},
                {"A2699", AirPodsModel::AirPodsPro2Lightning},
                {"A2698", AirPodsModel::AirPodsPro2Lightning},
                {"A3053", AirPodsModel::AirPods4},
                {"A3050", AirPodsModel::AirPods4},
                {"A3054", AirPodsModel::AirPods4},
                {"A3056", AirPodsModel::AirPods4ANC},
                {"A3055", AirPodsModel::AirPods4ANC},
                {"A3057", AirPodsModel::AirPods4ANC},
                // A3064 verified on a real device 2026-05-21; A3063 and A3065 are its published siblings.
                {"A3063", AirPodsModel::AirPodsPro3},
                {"A3064", AirPodsModel::AirPodsPro3},
                {"A3065", AirPodsModel::AirPodsPro3},
                // Powerbeats Pro: A2047 (Left), A2048 (Right), A2078 (Case), A3157
                {"A2047", AirPodsModel::PowerbeatsPro},
                {"A2048", AirPodsModel::PowerbeatsPro},
                {"A2078", AirPodsModel::PowerbeatsPro},
                {"A3157", AirPodsModel::PowerbeatsPro}};

            return modelNumberMap.value(modelNumber, AirPodsModel::Unknown);
        }

        // Return icons based on model
        inline QPair<QString, QString> getModelIcon(AirPodsModel model) {
            switch (model) {
                case AirPodsModel::AirPods1:
                case AirPodsModel::AirPods2:
                case AirPodsModel::PowerbeatsPro:
                    return {"pod.png", "pod_case.png"};
                case AirPodsModel::AirPods3:
                    return {"pod3.png", "pod3_case.png"};
                case AirPodsModel::AirPods4:
                case AirPodsModel::AirPods4ANC:
                    return {"pod3.png", "pod4_case.png"};
                case AirPodsModel::AirPodsPro:
                case AirPodsModel::AirPodsPro2Lightning:
                case AirPodsModel::AirPodsPro2USBC:
                case AirPodsModel::AirPodsPro3:
                    // Pro3 keeps the Pro silhouette; once a Pro3-specific
                    // asset is shipped, split this case.
                    return {"podpro.png", "podpro_case.png"};
                case AirPodsModel::AirPodsMaxLightning:
                case AirPodsModel::AirPodsMaxUSBC:
                case AirPodsModel::AirPodsMax2:
                    // AirPods Max has no physical charging case; the
                    // battery.hpp side never marks caseAvailable=true
                    // for headsets, so this slot is normally unused.
                    // Falling back to podmax.png instead of the missing
                    // `max_case.png` keeps Image.status = Ready if the
                    // QML ever does request it.
                    return {"podmax.png", "podmax.png"};
                default:
                    return {"pod.png", "pod_case.png"}; // Default icon for unknown models
            }
        }

        // User-facing model name for status surfaces (PodsMenu header
        // subtitle, openpods-ctl status, future MPRIS metadata). Maps the
        // internal enum to the marketing name Apple uses on the box.
        // Unknown -> empty string so the consumer can decide whether to
        // hide the slot or show a fallback.
        inline QString modelDisplayName(AirPodsModel model) {
            switch (model) {
                case AirPodsModel::AirPods1:               return QStringLiteral("AirPods");
                case AirPodsModel::AirPods2:               return QStringLiteral("AirPods (2nd generation)");
                case AirPodsModel::AirPods3:               return QStringLiteral("AirPods (3rd generation)");
                case AirPodsModel::AirPods4:               return QStringLiteral("AirPods 4");
                // Deliberately the same name as the plain AirPods 4: the panel says the family, and the capability keys say what it can do.
                case AirPodsModel::AirPods4ANC:            return QStringLiteral("AirPods 4");
                case AirPodsModel::AirPodsPro:             return QStringLiteral("AirPods Pro");
                case AirPodsModel::AirPodsPro2Lightning:   return QStringLiteral("AirPods Pro 2");
                case AirPodsModel::AirPodsPro2USBC:        return QStringLiteral("AirPods Pro 2 (USB-C)");
                case AirPodsModel::AirPodsMaxLightning:    return QStringLiteral("AirPods Max");
                case AirPodsModel::AirPodsMaxUSBC:         return QStringLiteral("AirPods Max (USB-C)");
                case AirPodsModel::AirPodsMax2:            return QStringLiteral("AirPods Max 2");
                case AirPodsModel::AirPodsPro3:            return QStringLiteral("AirPods Pro 3");
                case AirPodsModel::PowerbeatsPro:          return QStringLiteral("Powerbeats Pro");
                case AirPodsModel::Unknown:                return QString();
            }
            return QString();
        }

        // TODO: Only used for parseEncryptedPacket for battery status. Is it possible to determine this
        // from the data in the packet rather than by model? i.e number of batteries
        inline bool isModelHeadset(AirPodsModel model) {
            switch (model) {
                case AirPodsModel::AirPodsMaxLightning:
                case AirPodsModel::AirPodsMaxUSBC:
                case AirPodsModel::AirPodsMax2:
                    return true;
                default:
                    return false;
            }
        }

        // Published for every model but only meaningful where supportsNoiseControl is true; the Pro 3 accepts the Off packet and ignores it.
        inline bool supportsNoiseOff(AirPodsModel model) {
            return model != AirPodsModel::AirPodsPro3;
        }

        // Listening modes at all: AirPods 1, 2, 3 and the plain AirPods 4 have none.
        inline bool supportsNoiseControl(AirPodsModel model) {
            switch (model) {
                case AirPodsModel::AirPods4ANC:
                case AirPodsModel::AirPodsPro:
                case AirPodsModel::AirPodsPro2Lightning:
                case AirPodsModel::AirPodsPro2USBC:
                case AirPodsModel::AirPodsPro3:
                case AirPodsModel::AirPodsMaxLightning:
                case AirPodsModel::AirPodsMaxUSBC:
                case AirPodsModel::AirPodsMax2:
                case AirPodsModel::PowerbeatsPro:
                // Fail open: a model this map has not learned yet keeps the modes it had before.
                case AirPodsModel::Unknown:
                    return true;
                default:
                    return false;
            }
        }

        // Apple's own column set for Adaptive Audio and Conversation Awareness (apple.com/airpods/compare).
        inline bool hasH2ListeningFeatures(AirPodsModel model) {
            switch (model) {
                case AirPodsModel::AirPods4ANC:
                case AirPodsModel::AirPodsPro2Lightning:
                case AirPodsModel::AirPodsPro2USBC:
                case AirPodsModel::AirPodsPro3:
                case AirPodsModel::AirPodsMax2:
                    return true;
                default:
                    return false;
            }
        }

        // Not the Pro-series flag: the H1 parts with noise control, Pro 1 and Max 1, lack it.
        inline bool supportsAdaptiveAudio(AirPodsModel model) {
            return hasH2ListeningFeatures(model);
        }

        inline bool supportsConversationalAwareness(AirPodsModel model) {
            return hasH2ListeningFeatures(model);
        }

        // Apple's "Noise Cancellation with One AirPod", so it needs a second bud to keep in.
        inline bool supportsOneBudANC(AirPodsModel model) {
            switch (model) {
                case AirPodsModel::AirPods4ANC:
                case AirPodsModel::AirPodsPro:
                case AirPodsModel::AirPodsPro2Lightning:
                case AirPodsModel::AirPodsPro2USBC:
                case AirPodsModel::AirPodsPro3:
                case AirPodsModel::PowerbeatsPro:
                    return true;
                default:
                    return false;
            }
        }

        // Identity only since 2026-08-20: the Pro silhouette, and the flag older panels read.
        inline bool isProSeriesAirPods(AirPodsModel model) {
            switch (model) {
                case AirPodsModel::AirPodsPro:
                case AirPodsModel::AirPodsPro2Lightning:
                case AirPodsModel::AirPodsPro2USBC:
                case AirPodsModel::AirPodsPro3:
                    return true;
                default:
                    return false;
            }
        }

    }
}
