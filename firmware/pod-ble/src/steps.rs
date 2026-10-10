//! Step record buffer and pod clock, shared between the step producer and the BLE link.
//!
//! Records are held in RAM and lost on reset. TODO(comm): replace with flash-backed
//! storage, keep records until the app ACKs them, and persist `seq` across reboots.

use core::cell::Cell;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::channel::Channel;
use embassy_time::Instant;
use pod_protocol::StepRecord;

pub const QUEUE_LEN: usize = 64;

/// Records waiting to be sent to the app.
static QUEUE: Channel<CriticalSectionRawMutex, StepRecord, QUEUE_LEN> = Channel::new();

/// The record currently being sent. It stays here until the notification has gone
/// out, so a disconnect mid-send re-sends it next time (the app dedups by `seq`).
static IN_FLIGHT: Mutex<CriticalSectionRawMutex, Cell<Option<StepRecord>>> = Mutex::new(Cell::new(None));

/// UTC = uptime + offset, once the app has sent SET_TIME.
static UTC_OFFSET_S: Mutex<CriticalSectionRawMutex, Cell<Option<i64>>> = Mutex::new(Cell::new(None));

/// Queue a finished step for delivery. Drops it (with a warning) if the buffer is full.
pub fn push(rec: StepRecord) {
    if QUEUE.try_send(rec).is_err() {
        defmt::warn!("step buffer full, dropping seq {}", rec.seq);
    }
}

/// The next record to send: the in-flight one if a previous send didn't finish,
/// otherwise the oldest queued record. Waits if there is nothing to send.
pub async fn next() -> StepRecord {
    if let Some(rec) = IN_FLIGHT.lock(|c| c.get()) {
        return rec;
    }
    let rec = QUEUE.receive().await;
    IN_FLIGHT.lock(|c| c.set(Some(rec)));
    rec
}

/// Mark the record returned by [`next`] as delivered.
pub fn sent() {
    IN_FLIGHT.lock(|c| c.set(None));
}

/// Number of records still on the pod.
pub fn pending() -> u16 {
    let in_flight = IN_FLIGHT.lock(|c| c.get().is_some()) as usize;
    (QUEUE.len() + in_flight) as u16
}

pub fn set_utc(utc_s: u32) {
    let uptime = Instant::now().as_secs() as i64;
    UTC_OFFSET_S.lock(|c| c.set(Some(i64::from(utc_s) - uptime)));
}

/// Current timestamp for step records, and whether it is UTC (true) or uptime (false).
pub fn now() -> (u32, bool) {
    let uptime = Instant::now().as_secs() as i64;
    match UTC_OFFSET_S.lock(|c| c.get()) {
        Some(offset) => ((uptime + offset) as u32, true),
        None => (uptime as u32, false),
    }
}

/// Fake steps for bring-up: one per second, 30-60 %BW, as if from a 70 kg patient.
#[cfg(feature = "simulate-steps")]
#[embassy_executor::task]
pub async fn simulate() -> ! {
    use embassy_time::Timer;

    let mut seq: u32 = 0;
    let mut x: u32 = 0x9e37_79b9; // xorshift32 state; fake data doesn't need a real RNG
    let mut rand = move || {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        x
    };

    loop {
        Timer::after_secs(1).await;
        let (timestamp_s, synced) = now();
        let bw_permille = 300 + (rand() % 300) as u16;
        let peak_force_dn = (u32::from(bw_permille) * 7000 / 1000) as u16;
        let mut flags = 0;
        if bw_permille > 500 {
            flags |= StepRecord::FLAG_OVER_LIMIT;
        }
        if synced {
            flags |= StepRecord::FLAG_TIME_SYNCED;
        }
        push(StepRecord {
            seq,
            timestamp_s,
            peak_force_dn,
            peak_bw_permille: bw_permille,
            impulse_dns: peak_force_dn / 2,
            stance_ms: 550 + (rand() % 150) as u16,
            heel_pct: 35 + (rand() % 30) as u8,
            flags,
        });
        seq = seq.wrapping_add(1);
    }
}
