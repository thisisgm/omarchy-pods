---
type: reference
title: BlueZ reports no battery at all for AirPods
description: The device object carries no org.bluez.Battery1 interface, which is why a userspace daemon is a hard requirement rather than a convenience
tags: [bluez, airpods, battery]
status: stable
verified:
  - by: busctl introspect against a connected AirPods Pro 3 on Arch, BlueZ 5.x
    at: 2026-08-16
---

# What the bus actually offers

With AirPods Pro 3 paired, bonded, trusted and connected, the BlueZ device
object exposes exactly four interfaces:

```
org.bluez.Bearer.BREDR1
org.bluez.Bearer.LE1
org.bluez.Device1
org.bluez.MediaControl1
```

Asking for the battery fails outright:

```
$ busctl --system get-property org.bluez /org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF \
    org.bluez.Battery1 Percentage
Failed to get property Percentage on interface org.bluez.Battery1: No such interface 'org.bluez.Battery1'
```

BlueZ does publish `Battery1` for headsets that report battery through the
Handsfree Profile's `AT+IPHONEACCEV` or through the Battery Service over GATT.
AirPods do neither in a way BlueZ picks up on this box, so nothing on the
system knows the numbers.

# Why it matters here

This is the fact that ends the "could this be simpler" ladder. There is no
stock Omarchy command, no PipeWire property and no D-Bus interface that carries
per-pod or case battery. The only source is a userspace daemon speaking Apple's
AAP protocol, which is why pods is a requirement rather than one option
among several.

Per-pod battery is also not something a single BlueZ percentage could express:
left, right and case each report their own level and charging flag.
