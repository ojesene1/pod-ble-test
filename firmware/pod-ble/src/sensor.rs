//! FSR 400 force sensor read through the nRF54L15 SAADC (boilerplate).
//!
//! The FSR 400 (Interlink, P/N 30-49649) is a two-wire resistor: R drops as force rises
//! (~10 MOhm unloaded, ~30 kOhm at 20 g, ~6 kOhm at 100 g, ~1.2 kOhm at 1 kg, Fig. 1 of the
//! datasheet). It cannot be read as a digital GPIO; wire it as a voltage divider
//! (datasheet Fig. 2/3) and sample the middle node with the SAADC:
//!
//! ```text
//!   VDD ---[ FSR ]---+---[ RM = 470 ohm ]--- GND
//!                    |
//!                    +--> SENSOR_PIN (analog input)
//!
//!   Vout = VDD * RM / (RM + R_fsr)      (Vout rises with force)
//! ```
//!
//! Wiring notes:
//! - `RM` sets the sensitivity range. The datasheet plots 3k-100k. A bigger RM gives a larger
//!   signal at light loads but squashes the heavy-load end (Vout approaches VDD once the FSR
//!   falls well below RM). 470 ohm favours the heavy end: 20 g -> ~46 mV, 1 kg -> ~0.84 V,
//!   10 kg -> ~1.96 V at VDD = 3 V.
//!   If you change `RM`, change `RM_OHMS` below to match.
//! - The SAADC full scale with the settings below is 0.9 V / (2/8) = 3.6 V, above VDD, so the
//!   input cannot clip. Verify `VDD_MV` and the scale with a multimeter on the pin (see run()).
//! - Pin: `P1_11` (SAADC capable: P1.04-07, P1.11-14). P1.06/P1.07 carry the 32 kHz crystal
//!   and P1.04/P1.05 the DK's UART, so avoid those. Check P1.11 is free on your board.
//!
//! Hook-up needed in `main.rs` (not done here):
//! 1. `mod sensor;`
//! 2. Add `SAADC => embassy_nrf::saadc::InterruptHandler;` to the `bind_interrupts!` block.
//! 3. Build it and spawn the task, e.g. after the SDC is created:
//!    ```ignore
//!    let sensor = sensor::Sensor::new(p.SAADC, p.P1_11, Irqs).await;
//!    spawner.spawn(unwrap!(sensor::run(sensor)));
//!    ```

use embassy_nrf::interrupt::typelevel::Binding;
use embassy_nrf::interrupt::{self, InterruptExt, Priority};
use embassy_nrf::peripherals::{P1_11, SAADC};
use embassy_nrf::saadc::{self, ChannelConfig, Config, Oversample, Saadc, Time};
use embassy_nrf::Peri;
use embassy_time::{Duration, Ticker};

/// Supply voltage of the divider, mV. TODO: confirm for your board (or measure it).
const VDD_MV: u32 = 3300;
/// Measuring resistor to ground, ohms.
const RM_OHMS: u32 = 470;
/// SAADC full scale in mV: 0.9 V internal reference (nRF54L, not 0.6 V like nRF52) / gain 2/8.
const FULL_SCALE_MV: u32 = 3600;
/// 12-bit single-ended counts.
const COUNTS: u32 = 4096;
/// Sample period.
const SAMPLE_PERIOD: Duration = Duration::from_millis(10); // 100 Hz
/// Print one line every this many samples (10 -> 10 lines per second).
const PRINT_EVERY: u32 = 10;

/// (resistance in ohms, force in grams), read off the datasheet's typical curve (Fig. 1).
/// Approximate, sorted by falling resistance. TODO: replace with your own calibration
/// (press known weights on the actual insole/mount; the datasheet gives +-6 % part to part).
const CURVE: [(f32, f32); 5] = [
    (30_000.0, 20.0),
    (10_000.0, 50.0),
    (6_000.0, 100.0),
    (1_200.0, 1_000.0),
    (250.0, 10_000.0),
];

/// Below this the sensor is treated as unloaded (the FSR is ~10 MOhm unpressed).
const MIN_FORCE_G: f32 = 20.0;

pub struct Sensor {
    saadc: Saadc<'static, 1>,
}

impl Sensor {
    /// Set up the SAADC on `P1_11` and calibrate it.
    pub async fn new(
        saadc: Peri<'static, SAADC>,
        pin: Peri<'static, P1_11>,
        irq: impl Binding<interrupt::typelevel::SAADC, saadc::InterruptHandler> + 'static,
    ) -> Self {
        // The SoftDevice/MPSL reserve the highest interrupt priorities; keep SAADC below them.
        interrupt::SAADC.set_priority(Priority::P3);

        let mut ch = ChannelConfig::single_ended(pin);
        ch.time = Time::_40US; // longer acquisition for the high-impedance divider
        let mut cfg = Config::default(); // 12 bit
        cfg.oversample = Oversample::Over8x; // average 8 samples to cut noise

        let saadc = Saadc::new(saadc, irq, cfg, [ch]);
        saadc.calibrate().await;
        Self { saadc }
    }

    /// One raw conversion (0..4095).
    pub async fn read_raw(&mut self) -> u16 {
        let mut buf = [0i16; 1];
        self.saadc.sample(&mut buf).await;
        buf[0].max(0) as u16
    }

    /// Voltage at the divider midpoint, mV.
    pub async fn read_mv(&mut self) -> u32 {
        let raw = u32::from(self.read_raw().await);
        raw * FULL_SCALE_MV / COUNTS
    }

    /// FSR resistance in ohms, or `None` if unloaded / out of range.
    pub async fn read_fsr_ohms(&mut self) -> Option<u32> {
        let mv = self.read_mv().await;
        fsr_ohms(mv)
    }

    /// Force in 0.1 N units (the `peak_force_dn` unit of `pod_protocol::StepRecord`).
    pub async fn read_force_dn(&mut self) -> u16 {
        match self.read_fsr_ohms().await {
            Some(ohms) => force_dn(ohms),
            None => 0,
        }
    }
}

/// Divider math: Vout = VDD * RM / (RM + R)  =>  R = RM * (VDD - Vout) / Vout.
fn fsr_ohms(vout_mv: u32) -> Option<u32> {
    if vout_mv == 0 || vout_mv >= VDD_MV {
        return None; // open circuit (unloaded) or shorted
    }
    Some(RM_OHMS * (VDD_MV - vout_mv) / vout_mv)
}

/// Resistance -> force in grams, by linear interpolation on `CURVE`.
fn force_g(ohms: u32) -> f32 {
    let r = ohms as f32;
    if r >= CURVE[0].0 {
        0.0 // lighter than the curve's first point: treat as unloaded
    } else if r <= CURVE[CURVE.len() - 1].0 {
        CURVE[CURVE.len() - 1].1
    } else {
        let mut g = MIN_FORCE_G;
        for w in CURVE.windows(2) {
            let (r0, g0) = w[0];
            let (r1, g1) = w[1];
            if r <= r0 && r >= r1 {
                g = g0 + (g1 - g0) * (r0 - r) / (r0 - r1);
                break;
            }
        }
        g
    }
}

/// Resistance -> force in newtons.
fn force_n(ohms: u32) -> f32 {
    force_g(ohms) * 0.00981
}

/// Resistance -> force in 0.1 N units (the `peak_force_dn` unit of `pod_protocol::StepRecord`).
fn force_dn(ohms: u32) -> u16 {
    (force_g(ohms) * 0.0981) as u16
}

/// Samples the sensor and prints every reading to the terminal (defmt/RTT) at 10 Hz.
/// TODO: detect steps (threshold + peak/impulse/stance timing), then build a
/// `pod_protocol::StepRecord` and hand it over with `crate::steps::push(..)`.
#[embassy_executor::task]
pub async fn run(mut sensor: Sensor) -> ! {
    let mut ticker = Ticker::every(SAMPLE_PERIOD);
    let mut n: u32 = 0;
    loop {
        ticker.next().await;
        let raw = sensor.read_raw().await;
        n = n.wrapping_add(1);
        if n % PRINT_EVERY == 0 {
            let mv = u32::from(raw) * FULL_SCALE_MV / COUNTS;
            // Columns: force (N), resistance (ohm), voltage (mV), raw ADC counts.
            match fsr_ohms(mv) {
                Some(ohms) => {
                    let newtons = ((force_n(ohms) * 1000.0 + 0.5) as u32) as f32 / 1000.0;
                    defmt::info!("{=f32} N, {} ohm, {} mV, {}", newtons, ohms, mv, raw)
                }
                None => defmt::info!("0 N, open, {} mV, {}", mv, raw),
            }
        }
    }
}
