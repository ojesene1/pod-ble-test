#!/usr/bin/env python3
"""Decode pod BLE values copied from nRF Connect for Mobile, and build commands to send.

Byte layouts follow protocol/GATT.md (all multi-byte fields little-endian).
No dependencies beyond Python 3.8+.

Examples:
  python3 tools/pod_decode.py record "(0x) 0B-00-00-00-0C-00-00-00-5C-0C-C4-01-2E-06-64-02-2F-00"
  python3 tools/pod_decode.py status 00-00-0A-00-01
  python3 tools/pod_decode.py settime          # SET_TIME bytes for "now", to paste into nRF Connect
  python3 tools/pod_decode.py ack 258          # ACK bytes for seq 258
  python3 tools/pod_decode.py --self-test      # check against the golden vectors in GATT.md
"""

import argparse
import re
import struct
import sys
import time
from datetime import datetime, timezone

RECORD_LEN = 18
RECORD_FMT = "<IIHHHHBB"  # seq, timestamp, force, %BW, impulse, stance, heel, flags
STATUS_LEN = 5

FLAG_OVER_LIMIT = 0x01
FLAG_TIME_SYNCED = 0x02
FLAG_SENSOR_FAULT = 0x04

OP_SET_TIME = 0x01
OP_ACK = 0x02

# Golden vectors from protocol/GATT.md (also asserted by firmware/pod-protocol tests).
GOLDEN_RECORD = "02 01 00 00 54 c1 c3 6a c1 0d f7 01 e0 06 64 02 2a 03"
GOLDEN_STATUS = "00 00 07 00 01"
GOLDEN_SET_TIME = (1791213908, "01 54 c1 c3 6a")


def parse_hex(text: str) -> bytes:
    """Accept the formats nRF Connect shows: '(0x) 0B-00-...', '0B 00 ...', '0x0B00...', '0b00...'."""
    t = text.strip()
    t = re.sub(r"^\(0x\)\s*", "", t, flags=re.I)
    t = re.sub(r"^0x", "", t, flags=re.I)
    t = re.sub(r"[\s\-:,]", "", t)
    if not re.fullmatch(r"[0-9a-fA-F]*", t) or len(t) % 2:
        raise ValueError(f"not a hex byte string: {text!r}")
    return bytes.fromhex(t)


def hex_dash(b: bytes) -> str:
    return "-".join(f"{x:02X}" for x in b)


def decode_record(b: bytes) -> dict:
    if len(b) != RECORD_LEN:
        raise ValueError(f"a step record is {RECORD_LEN} bytes, got {len(b)}")
    seq, ts, force, bw, impulse, stance, heel, flags = struct.unpack(RECORD_FMT, b)
    return {
        "seq": seq,
        "timestamp_s": ts,
        "peak_force_n": force / 10,
        "peak_bw_pct": bw / 10,
        "impulse_ns": impulse / 10,
        "stance_ms": stance,
        "heel_pct": heel,
        "flags": flags,
    }


def format_record(r: dict) -> str:
    synced = bool(r["flags"] & FLAG_TIME_SYNCED)
    when = (
        datetime.fromtimestamp(r["timestamp_s"], tz=timezone.utc).astimezone().strftime("%Y-%m-%d %H:%M:%S %Z")
        if synced
        else f"{r['timestamp_s']} s since pod boot (clock not set)"
    )
    flags = [
        name
        for bit, name in ((FLAG_OVER_LIMIT, "OVER LIMIT"), (FLAG_TIME_SYNCED, "time synced"), (FLAG_SENSOR_FAULT, "SENSOR FAULT"))
        if r["flags"] & bit
    ]
    return "\n".join(
        [
            f"  seq          {r['seq']}",
            f"  time         {when}",
            f"  peak force   {r['peak_force_n']:.1f} N",
            f"  peak load    {r['peak_bw_pct']:.1f} % body weight",
            f"  impulse      {r['impulse_ns']:.1f} N*s",
            f"  stance       {r['stance_ms']} ms",
            f"  heel share   {r['heel_pct']} %",
            f"  flags        0x{r['flags']:02X} ({', '.join(flags) or 'none'})",
        ]
    )


def decode_status(b: bytes) -> dict:
    if len(b) < STATUS_LEN:
        raise ValueError(f"status is {STATUS_LEN} bytes, got {len(b)}")
    proto, rec_ver, pending, flags = struct.unpack("<BBHB", b[:STATUS_LEN])
    return {"protocol_version": proto, "record_version": rec_ver, "pending": pending, "time_synced": bool(flags & 1)}


def encode_u32_command(op: int, value: int) -> bytes:
    return struct.pack("<BI", op, value)


def self_test() -> None:
    r = decode_record(parse_hex(GOLDEN_RECORD))
    assert r == {
        "seq": 258,
        "timestamp_s": 1791213908,
        "peak_force_n": 352.1,
        "peak_bw_pct": 50.3,
        "impulse_ns": 176.0,
        "stance_ms": 612,
        "heel_pct": 42,
        "flags": FLAG_OVER_LIMIT | FLAG_TIME_SYNCED,
    }, r
    assert decode_status(parse_hex(GOLDEN_STATUS)) == {
        "protocol_version": 0,
        "record_version": 0,
        "pending": 7,
        "time_synced": True,
    }
    utc, expected = GOLDEN_SET_TIME
    assert encode_u32_command(OP_SET_TIME, utc) == parse_hex(expected)
    assert parse_hex("(0x) 00-00-07-00-01") == parse_hex("0x0000070001") == bytes([0, 0, 7, 0, 1])
    print("self-test passed: decoder matches the golden vectors in protocol/GATT.md")


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--self-test", action="store_true", help="check the decoder against the golden vectors")
    sub = p.add_subparsers(dest="cmd")
    s = sub.add_parser("record", help="decode a Step Record notification (fbe30002)")
    s.add_argument("hex", nargs="+")
    s = sub.add_parser("status", help="decode a Status read (fbe30004)")
    s.add_argument("hex", nargs="+")
    s = sub.add_parser("settime", help="print SET_TIME bytes to write to the Control Point (fbe30003)")
    s.add_argument("--utc", type=int, help="epoch seconds (default: now)")
    s = sub.add_parser("ack", help="print ACK bytes to write to the Control Point (fbe30003)")
    s.add_argument("seq", type=int)
    args = p.parse_args()

    if args.self_test:
        self_test()
        return 0

    try:
        if args.cmd == "record":
            data = parse_hex(" ".join(args.hex))
            if not data or len(data) % RECORD_LEN:
                raise ValueError(f"expected a multiple of {RECORD_LEN} bytes, got {len(data)}")
            for i in range(0, len(data), RECORD_LEN):
                print(f"Step record {i // RECORD_LEN + 1}:" if len(data) > RECORD_LEN else "Step record:")
                print(format_record(decode_record(data[i : i + RECORD_LEN])))
        elif args.cmd == "status":
            st = decode_status(parse_hex(" ".join(args.hex)))
            print("Status:")
            print(f"  protocol version   {st['protocol_version']}")
            print(f"  record version     {st['record_version']}")
            print(f"  records pending    {st['pending']}")
            print(f"  clock set          {'yes' if st['time_synced'] else 'no'}")
        elif args.cmd == "settime":
            utc = args.utc if args.utc is not None else int(time.time())
            b = encode_u32_command(OP_SET_TIME, utc)
            print(f"Write to Control Point (fbe30003) as BYTE ARRAY:\n  {b.hex().upper()}")
            print(f"  (= {hex_dash(b)}, SET_TIME {utc})")
        elif args.cmd == "ack":
            b = encode_u32_command(OP_ACK, args.seq)
            print(f"Write to Control Point (fbe30003) as BYTE ARRAY:\n  {b.hex().upper()}")
            print(f"  (= {hex_dash(b)}, ACK seq {args.seq})")
        else:
            p.print_help()
            return 1
    except ValueError as e:
        print(f"error: {e}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
