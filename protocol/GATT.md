# Pod ↔ App BLE Protocol (v0, draft)

This file is the contract between the pod firmware and whatever connects to it: nRF Connect for Mobile today, and the patient app later. The contract also lives in code, and every copy must change together, in the same commit:

| Copy | Where |
|---|---|
| This document | `protocol/GATT.md` |
| Firmware (Rust) | `firmware/pod-protocol/src/lib.rs` |
| Test decoder (Python) | `tools/pod_decode.py` |

The Rust unit tests and `python3 tools/pod_decode.py --self-test` both check the **golden vectors** at the bottom of this page, so if the copies drift apart, a test fails.

## Advertising
| Field | Value |
|---|---|
| Advertising data | Flags + complete list of 128-bit service UUIDs (the Step Record Service) |
| Scan response | Complete local name `WB-Pod` |
| Address | Static random, derived from the chip's factory ID (unique and stable per pod) |
| Connectable | Yes, one central at a time |

The app should **filter scans by the service UUID**, not by name. The UUID is in the main advertising packet, so this also works for iOS background scans.

## Step Record Service: `fbe30001-fc86-4926-b9db-293cbba6fc2f`
All custom UUIDs share the base `fbe3xxxx-fc86-4926-b9db-293cbba6fc2f`.

| Characteristic | UUID | Properties | Payload |
|---|---|---|---|
| Step Record | `fbe30002-…` | Notify | one 18-byte record per notification |
| Control Point | `fbe30003-…` | Write | opcode byte + payload |
| Status | `fbe30004-…` | Read | 5 bytes |

All multi-byte fields are **little-endian**.

### Step Record (18 bytes)
| Offset | Type | Field | Units / notes |
|---|---|---|---|
| 0 | u32 | `seq` | Sequence number, used to detect gaps and duplicates. Restarts at 0 on pod reboot until flash storage exists |
| 4 | u32 | `timestamp_s` | UTC epoch seconds if flag bit1 is set, otherwise seconds since pod boot |
| 8 | u16 | `peak_force_dn` | Peak total force, 0.1 N |
| 10 | u16 | `peak_bw_permille` | Peak force as % body weight × 10 (503 = 50.3 %BW) |
| 12 | u16 | `impulse_dns` | Force–time integral, 0.1 N·s |
| 14 | u16 | `stance_ms` | Stance duration, ms |
| 16 | u8 | `heel_pct` | Heel zone's share of peak load, 0–100 |
| 17 | u8 | `flags` | bit0 over-limit · bit1 time synced · bit2 sensor fault |

The field set follows step 7 of the firmware pipeline in the sensor-selection notes. **It still needs agreement from Platforms before the backend schema is fixed.**

### Control Point opcodes
| Opcode | Name | Payload | Status |
|---|---|---|---|
| `0x01` | SET_TIME | u32 UTC epoch seconds | Implemented. The app sends this on every connect |
| `0x02` | ACK | u32 last `seq` the app has stored durably | Accepted, but does nothing until flash storage exists |

Errors: a wrong payload length returns ATT `0x0D` (Invalid Attribute Value Length), an unknown opcode returns ATT `0x06` (Request Not Supported), and a non-zero offset returns ATT `0x07`.

### Status (5 bytes)
| Offset | Type | Field |
|---|---|---|
| 0 | u8 | protocol version (0) |
| 1 | u8 | step record version (0) |
| 2 | u16 | records still on the pod |
| 4 | u8 | flags: bit0 time synced |

## Expected app flow
1. Scan, filtered by the Step Record Service UUID, then connect. On Android, the app requests a 512-byte MTU; iOS negotiates the MTU itself. An 18-byte record fits even the default 23-byte MTU.
2. Write `SET_TIME` to the Control Point.
3. Read Status and check that the versions match what the app understands.
4. Enable notifications on Step Record. The pod drains its buffer first, then streams live steps.
5. Persist each record, keyed by (`pod id`, `seq`). Write `ACK` only after the record is stored durably.

**Delivery is at-least-once.** A record that was being sent when the link dropped is sent again on the next connection, so the app must dedup.

## Golden vectors
Step record `{seq: 258, timestamp_s: 1791213908, peak_force_dn: 3521, peak_bw_permille: 503, impulse_dns: 1760, stance_ms: 612, heel_pct: 42, flags: over-limit | time-synced}`:
```
02 01 00 00 54 c1 c3 6a c1 0d f7 01 e0 06 64 02 2a 03
```
Status `{protocol 0, record 0, pending 7, time synced}`: `00 00 07 00 01`
SET_TIME 1791213908: `01 54 c1 c3 6a`

## Open items (Communication team)
- **Security:** characteristics are unencrypted for bring-up. Before any real patient data is sent, require an encrypted, bonded link (LE Secure Connections) on every Step Record Service characteristic, and persist bonds in flash. TrouBLE's `security` feature provides pairing.
- **Reliability:** records live in a 64-entry RAM queue and are lost on reset. Move them to flash, delete them on `ACK`, persist `seq` across reboots, and add a "replay from seq N" opcode for filling gaps.
- **Throughput:** batch several records per notification once the MTU is negotiated. `tools/pod_decode.py` already accepts multiples of 18 bytes.
- **Device info:** firmware and hardware revision. Either add the standard Device Information Service (0x180A) or extend Status.
- **Wear/activity records:** hourly wear-time summaries (pipeline step 9) need their own characteristic or record type.
