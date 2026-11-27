---
type: reference
title: How AirPods Pro 3 present themselves to a Linux box
description: The BlueZ identity, the AAP service UUID the daemon connects to, and the model code that drives Pro-only features
tags: [airpods, bluetooth, aap]
status: stable
verified:
  - by: bluetoothctl info and pactl against a connected AirPods Pro 3 on Arch
    at: 2026-08-16
---

# What BlueZ sees

```
Device AA:BB:CC:DD:EE:FF (public)
	Name: GM’s AirPods Pro
	Class: 0x00240418
	Icon: audio-headphones
	Modalias: bluetooth:v004Cp2027d0429
```

Vendor `0x004C` is Apple, product `0x2027`. Note that the advertised **name
says "AirPods Pro", not "Pro 3"**, so the generation cannot be read off the
name. It comes from the AAP metadata packet as a model number, `A3064` on this
device, which pods maps to `AirPodsPro3` and therefore to
`is_pro_series: true`.

# The service the daemon needs

Among the device's UUIDs are two vendor-specific entries:

```
4715650b-5e9d-4ac2-b898-a4fc0aa5df78
74ec2172-0bad-4d01-8f77-997b2be0722a
```

The daemon connects an L2CAP `QBluetoothSocket` to the second one. If that UUID
is absent from `bluetoothctl info`, the AAP path is not available and no amount
of daemon configuration will produce battery or listening-mode state.

# Audio

The A2DP sink appears as `bluez_output.AA_BB_CC_DD_EE_FF.1`, an ordinary
PipeWire sink that the stock Omarchy audio panel switches like any other. That
is the reason volume and output selection stay out of this plugin: they are
already solved, generically, one panel over.

# Two paths, two clocks

Battery, in-ear and lid state arrive over the BLE advertisement, while noise
mode and the control verbs go over L2CAP. The two can disagree in time: the
pods can be sitting in a closed case, reporting battery over BLE, while
`connected` is false because the audio link is down.
