#ifndef IDLESCAN_HPP
#define IDLESCAN_HPP

#include <QSettings>

// The idle BLE scan is an active discovery session, and while one is open the kernel never arms
// the passive scan that bonded LE devices reconnect through, so a mouse or keyboard stays dead
// for as long as the pods are away. Off, battery, lid and ear state come over the control link
// only, and are unknown while the pods are disconnected.
inline bool idleScanEnabled(const QSettings &settings)
{
    return settings.value("bluetooth/idleScan", true).toBool();
}

#endif // IDLESCAN_HPP
