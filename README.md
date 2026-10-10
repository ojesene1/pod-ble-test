# Pod BLE Test

Bench test of the BLE link between the pod (on the **nRF54L15 DK**) and a phone running **nRF Connect for Mobile**. There's no custom app: the phone side is Nordic's free app, and you read the data by hand or with the included decoder.

The firmware advertises as `WB-Pod` and sends a fake step record every second, so you can test the whole link without the insole or sensors.

```
Pod_BLE_TEST/
├── firmware/
│   ├── pod-ble/          firmware for the nRF54L15 (Rust, Embassy + TrouBLE)
│   └── pod-protocol/     byte formats for the BLE data, with unit tests
├── protocol/GATT.md      the spec: UUIDs, byte layouts, golden test vectors
└── tools/pod_decode.py   turns hex from nRF Connect into readable values, builds commands
```

---

## 1. One-time setup

All commands run in **WSL (Ubuntu)** unless marked *Windows*.

**Rust:**
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```
The exact toolchain version and the nRF54L15 target install themselves on the first build (from `firmware/pod-ble/rust-toolchain.toml`).

**probe-rs** (flashes the board and shows its logs):
```bash
cargo install cargo-binstall
cargo binstall probe-rs-tools
```

**clang**, which the Nordic radio library needs at build time:
```bash
sudo apt install clang
```

**Let probe-rs see USB devices** (once):
```bash
curl -LO https://probe.rs/files/69-probe-rs.rules
sudo cp 69-probe-rs.rules /etc/udev/rules.d/
sudo service udev start
sudo udevadm control --reload
sudo udevadm trigger
```

*Windows:* install usbipd-win, which passes the DK's USB connection through to WSL:
```powershell
winget install --interactive --exact dorssel.usbipd-win
```

**Phone:** install **nRF Connect for Mobile** (iOS App Store / Google Play).

## 2. Connect the DK to WSL (every session)

Plug in the DK. In an **Administrator** PowerShell (*Windows*):
```powershell
usbipd list                                     # find the "J-Link" line, note its BUSID, e.g. 2-3
usbipd bind --busid 2-3                         # first time only
usbipd attach --wsl --busid 2-3 --auto-attach   # keep this window open
```
Then check from WSL:
```bash
probe-rs list        # should show a J-Link
```

## 3. Build and flash

```bash
cd firmware/pod-ble
cargo run --release
```
The first build takes a few minutes; later builds take seconds. After flashing, the terminal stays open and shows logs from the board:
```
INFO  simulated steps enabled
INFO  [adv] advertising as "WB-Pod"
```
Press Ctrl+C to stop watching logs. The board keeps running.

## 4. Test with nRF Connect for Mobile

Keep the terminal open next to the phone. Each step below has something to check on both.

### a. Advertising
In the **Scanner** tab, find **WB-Pod** (filter by name if the list is busy). Tap the row to expand it. You should see:
- **Complete List of 128-bit Service UUIDs:** `FBE30001-FC86-4926-B9DB-293CBBA6FC2F`
- **Complete Local Name:** `WB-Pod`

### b. Connect
Tap **Connect**. The terminal should show `[adv] connected`. Expand **Unknown Service** `FBE30001-…`, which has three characteristics:

| UUID | Name | Property | Icon |
|---|---|---|---|
| `FBE30002-…` | Step Record | NOTIFY | multiple down-arrows = subscribe |
| `FBE30003-…` | Control Point | WRITE | up arrow = send |
| `FBE30004-…` | Status | READ | single down arrow = read |

### c. Stream step records
Subscribe on `FBE30002`. A new 18-byte value should arrive **every second**. To decode one, copy it and run:
```bash
python3 tools/pod_decode.py record "(0x) 0B-00-00-00-0C-00-00-00-5C-0C-C4-01-2E-06-64-02-2F-00"
```
```
Step record:
  seq          11
  time         12 s since pod boot (clock not set)
  peak force   316.4 N
  peak load    45.2 % body weight
  ...
```
Check that `seq` goes up by 1 with each value, and that the flags show **OVER LIMIT** whenever the load is over 50 %.

### d. Buffering
Unsubscribe from `FBE30002`, wait about 10 seconds, then read `FBE30004` (Status):
```bash
python3 tools/pod_decode.py status 00-00-0A-00-00
```
`records pending` should be about 10. Subscribe again: those records arrive in a burst, with no gaps in `seq`, and then it continues at one per second.

### e. Set the pod's clock
Generate the command for the current time:
```bash
python3 tools/pod_decode.py settime
```
On `FBE30003`, tap the up arrow, choose **BYTE ARRAY**, paste the hex and send. Then check:
- Terminal: `[ctrl] time set to …`
- New step records decode with a real date/time and the flag **time synced**
- Status: `clock set: yes`

### f. Error handling
Write just `01` to `FBE30003`. The phone should show a write error (ATT 0x0D, wrong length), and the board keeps running.

### g. Reconnect
Disconnect in the app. The terminal should show `[gatt] disconnected`, and WB-Pod should reappear in the Scanner within a second or two. Reconnect and subscribe: buffered records arrive, then live ones.

If a through g all pass, the pod side of the BLE link works.

### Record your results

| Test | Pass? | Notes |
|---|---|---|
| a. Advertising visible | | |
| b. Connects, 3 characteristics shown | | |
| c. Records every 1 s, seq +1 | | |
| d. Buffered records delivered, no seq gaps | | |
| e. SET_TIME applied | | |
| f. Bad write rejected | | |
| g. Re-advertises after disconnect | | |

## Troubleshooting

| Symptom | Fix |
|---|---|
| `No connected probes were found` | The DK isn't attached to WSL. Redo step 2, then run `probe-rs list` |
| Probe listed but "permission denied" | Redo the udev step in section 1, then unplug and replug the DK |
| Flashed, but no logs appear | Run `cargo run --release` again, or press the DK's RESET button. The first attach sometimes misses the log stream |
| WB-Pod not in Scanner | Check the terminal shows `advertising`. On Android, give nRF Connect Bluetooth and Location permission |
| Build error about `libclang` | Run `sudo apt install clang` |

## Options

- **Turn off fake steps:** `cargo run --release --no-default-features`
- **Run the protocol unit tests** (no hardware needed): `cd firmware/pod-protocol && cargo test`
- **Check the decoder against the spec:** `python3 tools/pod_decode.py --self-test`

## Changing the protocol

If you change the step-record layout, a UUID or a command, update all three copies together, in the same commit:
- `protocol/GATT.md`
- `firmware/pod-protocol/src/lib.rs`
- `tools/pod_decode.py`

Then run both test commands above.
