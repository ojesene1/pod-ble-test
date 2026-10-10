//! On-air formats for the pod <-> app BLE link.
//!
//! This crate is the firmware's copy of the contract in `protocol/GATT.md`;
//! `tools/pod_decode.py` is the decoder used when testing with nRF Connect.
//! Any change here is a protocol change: bump the version constants and update the
//! doc, the decoder and the golden test vectors together.
//!
//! All multi-byte fields are little-endian.

#![no_std]

/// Bumped on any change to the service layout or control-point opcodes.
pub const PROTOCOL_VERSION: u8 = 0;

/// 128-bit UUIDs. Base: `fbe3xxxx-fc86-4926-b9db-293cbba6fc2f`.
pub mod uuid {
    pub const STEP_SERVICE: &str = "fbe30001-fc86-4926-b9db-293cbba6fc2f";
    pub const STEP_RECORD: &str = "fbe30002-fc86-4926-b9db-293cbba6fc2f";
    pub const CONTROL_POINT: &str = "fbe30003-fc86-4926-b9db-293cbba6fc2f";
    pub const STATUS: &str = "fbe30004-fc86-4926-b9db-293cbba6fc2f";

    /// Step Record Service UUID in the little-endian byte order used in advertising data.
    pub const STEP_SERVICE_LE: [u8; 16] = [
        0x2f, 0xfc, 0xa6, 0xbb, 0x3c, 0x29, 0xdb, 0xb9, 0x26, 0x49, 0x86, 0xfc, 0x01, 0x00, 0xe3, 0xfb,
    ];
}

/// Per-step summary sent as one notification on the Step Record characteristic.
///
/// DRAFT v0: field set follows step 7 of the firmware pipeline notes; still to be
/// agreed with Platforms before the backend schema is fixed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StepRecord {
    /// Sequence number. Monotonic within one boot (persisted across reboots once flash storage lands).
    pub seq: u32,
    /// UTC epoch seconds if [`StepRecord::FLAG_TIME_SYNCED`] is set, otherwise seconds since boot.
    pub timestamp_s: u32,
    /// Peak total force, 0.1 N units.
    pub peak_force_dn: u16,
    /// Peak force as % body weight x10 (503 = 50.3 %BW).
    pub peak_bw_permille: u16,
    /// Force-time integral, 0.1 N*s units.
    pub impulse_dns: u16,
    /// Stance duration, ms.
    pub stance_ms: u16,
    /// Heel zone's share of peak load, 0-100.
    pub heel_pct: u8,
    /// `FLAG_*` bits.
    pub flags: u8,
}

impl StepRecord {
    pub const VERSION: u8 = 0;
    pub const LEN: usize = 18;

    pub const FLAG_OVER_LIMIT: u8 = 1 << 0;
    pub const FLAG_TIME_SYNCED: u8 = 1 << 1;
    pub const FLAG_SENSOR_FAULT: u8 = 1 << 2;

    pub fn to_bytes(&self) -> [u8; Self::LEN] {
        let mut b = [0u8; Self::LEN];
        b[0..4].copy_from_slice(&self.seq.to_le_bytes());
        b[4..8].copy_from_slice(&self.timestamp_s.to_le_bytes());
        b[8..10].copy_from_slice(&self.peak_force_dn.to_le_bytes());
        b[10..12].copy_from_slice(&self.peak_bw_permille.to_le_bytes());
        b[12..14].copy_from_slice(&self.impulse_dns.to_le_bytes());
        b[14..16].copy_from_slice(&self.stance_ms.to_le_bytes());
        b[16] = self.heel_pct;
        b[17] = self.flags;
        b
    }

    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        if b.len() != Self::LEN {
            return None;
        }
        Some(Self {
            seq: u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            timestamp_s: u32::from_le_bytes([b[4], b[5], b[6], b[7]]),
            peak_force_dn: u16::from_le_bytes([b[8], b[9]]),
            peak_bw_permille: u16::from_le_bytes([b[10], b[11]]),
            impulse_dns: u16::from_le_bytes([b[12], b[13]]),
            stance_ms: u16::from_le_bytes([b[14], b[15]]),
            heel_pct: b[16],
            flags: b[17],
        })
    }
}

/// Value of the Status characteristic (read).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PodStatus {
    pub protocol_version: u8,
    pub record_version: u8,
    /// Records waiting on the pod.
    pub pending: u16,
    /// bit0: time synced.
    pub flags: u8,
}

impl PodStatus {
    pub const LEN: usize = 5;
    pub const FLAG_TIME_SYNCED: u8 = 1 << 0;

    pub fn new(pending: u16, time_synced: bool) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            record_version: StepRecord::VERSION,
            pending,
            flags: if time_synced { Self::FLAG_TIME_SYNCED } else { 0 },
        }
    }

    pub fn to_bytes(&self) -> [u8; Self::LEN] {
        let p = self.pending.to_le_bytes();
        [self.protocol_version, self.record_version, p[0], p[1], self.flags]
    }

    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        if b.len() < Self::LEN {
            return None;
        }
        Some(Self {
            protocol_version: b[0],
            record_version: b[1],
            pending: u16::from_le_bytes([b[2], b[3]]),
            flags: b[4],
        })
    }
}

/// Commands the app writes to the Control Point characteristic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlCommand {
    /// `0x01` + u32 UTC epoch seconds.
    SetTime { utc_s: u32 },
    /// `0x02` + u32 last `seq` the app has stored durably.
    Ack { seq: u32 },
}

/// Why a Control Point write was rejected. Maps to an ATT error code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlError {
    /// Wrong payload length for the opcode (ATT 0x0D Invalid Attribute Value Length).
    BadLength,
    /// Unknown opcode (ATT 0x06 Request Not Supported).
    UnknownOpcode,
}

impl ControlCommand {
    pub const OP_SET_TIME: u8 = 0x01;
    pub const OP_ACK: u8 = 0x02;
    pub const MAX_LEN: usize = 5;

    pub fn parse(b: &[u8]) -> Result<Self, ControlError> {
        let (&op, payload) = b.split_first().ok_or(ControlError::BadLength)?;
        let u32_arg = |p: &[u8]| -> Result<u32, ControlError> {
            let a: [u8; 4] = p.try_into().map_err(|_| ControlError::BadLength)?;
            Ok(u32::from_le_bytes(a))
        };
        match op {
            Self::OP_SET_TIME => Ok(Self::SetTime { utc_s: u32_arg(payload)? }),
            Self::OP_ACK => Ok(Self::Ack { seq: u32_arg(payload)? }),
            _ => Err(ControlError::UnknownOpcode),
        }
    }

    pub fn to_bytes(&self) -> [u8; 5] {
        let (op, arg) = match *self {
            Self::SetTime { utc_s } => (Self::OP_SET_TIME, utc_s),
            Self::Ack { seq } => (Self::OP_ACK, seq),
        };
        let a = arg.to_le_bytes();
        [op, a[0], a[1], a[2], a[3]]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Golden vector shared with protocol/GATT.md and tools/pod_decode.py.
    const GOLDEN_RECORD: StepRecord = StepRecord {
        seq: 258,
        timestamp_s: 1_791_213_908,
        peak_force_dn: 3521,
        peak_bw_permille: 503,
        impulse_dns: 1760,
        stance_ms: 612,
        heel_pct: 42,
        flags: StepRecord::FLAG_OVER_LIMIT | StepRecord::FLAG_TIME_SYNCED,
    };
    const GOLDEN_RECORD_BYTES: [u8; 18] = [
        0x02, 0x01, 0x00, 0x00, 0x54, 0xc1, 0xc3, 0x6a, 0xc1, 0x0d, 0xf7, 0x01, 0xe0, 0x06, 0x64, 0x02, 0x2a, 0x03,
    ];

    #[test]
    fn step_record_golden_vector() {
        assert_eq!(GOLDEN_RECORD.to_bytes(), GOLDEN_RECORD_BYTES);
        assert_eq!(StepRecord::from_bytes(&GOLDEN_RECORD_BYTES), Some(GOLDEN_RECORD));
    }

    #[test]
    fn step_record_rejects_wrong_length() {
        assert_eq!(StepRecord::from_bytes(&GOLDEN_RECORD_BYTES[..17]), None);
        assert_eq!(StepRecord::from_bytes(&[0u8; 19]), None);
    }

    #[test]
    fn status_golden_vector() {
        let s = PodStatus::new(7, true);
        assert_eq!(s.to_bytes(), [0x00, 0x00, 0x07, 0x00, 0x01]);
        assert_eq!(PodStatus::from_bytes(&s.to_bytes()), Some(s));
    }

    #[test]
    fn control_commands_round_trip() {
        let set = ControlCommand::SetTime { utc_s: 1_791_213_908 };
        assert_eq!(set.to_bytes(), [0x01, 0x54, 0xc1, 0xc3, 0x6a]);
        assert_eq!(ControlCommand::parse(&set.to_bytes()), Ok(set));
        let ack = ControlCommand::Ack { seq: 258 };
        assert_eq!(ControlCommand::parse(&ack.to_bytes()), Ok(ack));
    }

    #[test]
    fn control_rejects_bad_input() {
        assert_eq!(ControlCommand::parse(&[]), Err(ControlError::BadLength));
        assert_eq!(ControlCommand::parse(&[0x01, 0x00]), Err(ControlError::BadLength));
        assert_eq!(ControlCommand::parse(&[0x7f, 0, 0, 0, 0]), Err(ControlError::UnknownOpcode));
    }

    #[test]
    fn service_uuid_le_matches_string() {
        let hex: [u8; 32] = {
            let mut out = [0u8; 32];
            let mut i = 0;
            for c in uuid::STEP_SERVICE.bytes().filter(|c| *c != b'-') {
                out[i] = c;
                i += 1;
            }
            out
        };
        let nib = |c: u8| (c as char).to_digit(16).unwrap() as u8;
        let mut be = [0u8; 16];
        for i in 0..16 {
            be[i] = nib(hex[2 * i]) << 4 | nib(hex[2 * i + 1]);
        }
        be.reverse();
        assert_eq!(be, uuid::STEP_SERVICE_LE);
    }
}
