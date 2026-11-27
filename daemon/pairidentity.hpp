#pragma once

#include <QString>

#include "enums.h"

// Decides which pair the name, model and battery the daemon holds belong to, when more than one pair is around.
namespace PairIdentity
{
    using AirpodsTrayApp::Enums::AirPodsModel;

    // An empty owner means the state came from settings or BLE, not from a control link, so it cannot be trusted for this pair.
    inline bool stateBelongsTo(const QString &stateOwner, const QString &connectingAddress)
    {
        return !stateOwner.isEmpty()
            && stateOwner.compare(connectingAddress, Qt::CaseInsensitive) == 0;
    }

    // The stored IRK resolves only the pair that handed it over, which need not be the pair that is connected.
    inline bool advertIsFromOtherPair(bool pairConnected, AirPodsModel connectedModel, AirPodsModel advertModel)
    {
        return pairConnected
            && connectedModel != AirPodsModel::Unknown
            && advertModel != AirPodsModel::Unknown
            && advertModel != connectedModel;
    }
}
