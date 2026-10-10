//! GATT server and connection handling for the Step Record Service.
//! The on-air contract is protocol/GATT.md; byte layouts live in the `pod-protocol` crate.

use defmt::{info, warn};
use embassy_futures::join::join;
use embassy_futures::select::select;
use embassy_time::Timer;
use pod_protocol::{uuid, ControlCommand, ControlError, PodStatus};
use trouble_host::prelude::*;

use crate::steps;

pub const DEVICE_NAME: &str = "WB-Pod";

const CONNECTIONS_MAX: usize = 1;
const L2CAP_CHANNELS_MAX: usize = 2; // signalling + ATT

#[gatt_server]
struct Server {
    step_service: StepService,
}

// UUID literals must match pod_protocol::uuid (the macro needs literals).
#[gatt_service(uuid = "fbe30001-fc86-4926-b9db-293cbba6fc2f")]
struct StepService {
    /// One 18-byte StepRecord per notification.
    #[characteristic(uuid = "fbe30002-fc86-4926-b9db-293cbba6fc2f", notify)]
    record: [u8; 18],
    /// Opcode + payload, see ControlCommand.
    #[characteristic(uuid = "fbe30003-fc86-4926-b9db-293cbba6fc2f", write)]
    control: [u8; 5],
    /// 5-byte PodStatus, refreshed on every read.
    #[characteristic(uuid = "fbe30004-fc86-4926-b9db-293cbba6fc2f", read)]
    status: [u8; 5],
}

/// Run the BLE host forever: advertise, serve one connection at a time, repeat.
pub async fn run<C: Controller>(controller: C, address: Address) {
    let mut resources: HostResources<DefaultPacketPool, CONNECTIONS_MAX, L2CAP_CHANNELS_MAX> = HostResources::new();
    let stack = trouble_host::new(controller, &mut resources)
        .set_random_address(address)
        .build();
    let runner = stack.runner();
    let mut peripheral = stack.peripheral();

    let server = Server::new_with_config(GapConfig::Peripheral(PeripheralConfig {
        name: DEVICE_NAME,
        appearance: &appearance::sensor::GENERIC_SENSOR,
    }))
    .unwrap();

    let _ = join(host_task(runner), async {
        loop {
            match advertise(&mut peripheral, &server).await {
                Ok(conn) => {
                    // Both futures end when the link drops; then we go back to advertising.
                    select(gatt_events_task(&server, &conn), stream_steps_task(&server, &conn)).await;
                }
                Err(e) => panic!("[adv] error: {:?}", defmt::Debug2Format(&e)),
            }
        }
    })
    .await;
}

async fn host_task<C: Controller, P: PacketPool>(mut runner: Runner<'_, C, P>) {
    loop {
        if let Err(e) = runner.run().await {
            panic!("[host] error: {:?}", defmt::Debug2Format(&e));
        }
    }
}

/// Advertise until a central connects.
///
/// The service UUID goes in the advertising packet (so phones can filter scans by it,
/// including iOS background scans); the name goes in the scan response.
async fn advertise<'values, 'server, C: Controller>(
    peripheral: &mut Peripheral<'values, C, DefaultPacketPool>,
    server: &'server Server<'values>,
) -> Result<GattConnection<'values, 'server, DefaultPacketPool>, BleHostError<C::Error>> {
    let mut adv_data = [0; 31];
    let adv_len = AdStructure::encode_slice(
        &[
            AdStructure::Flags(LE_GENERAL_DISCOVERABLE | BR_EDR_NOT_SUPPORTED),
            AdStructure::CompleteServiceUuids128(&[uuid::STEP_SERVICE_LE]),
        ],
        &mut adv_data[..],
    )?;
    let mut scan_data = [0; 31];
    let scan_len = AdStructure::encode_slice(
        &[AdStructure::CompleteLocalName(DEVICE_NAME.as_bytes())],
        &mut scan_data[..],
    )?;

    let advertiser = peripheral
        .advertise(
            &Default::default(),
            Advertisement::ConnectableScannableUndirected {
                adv_data: &adv_data[..adv_len],
                scan_data: &scan_data[..scan_len],
            },
        )
        .await?;
    info!("[adv] advertising as \"{}\"", DEVICE_NAME);
    let conn = advertiser.accept().await?.with_attribute_server(server)?;
    info!("[adv] connected");
    Ok(conn)
}

/// Handle reads/writes from the app until the link drops.
async fn gatt_events_task<P: PacketPool>(server: &Server<'_>, conn: &GattConnection<'_, '_, P>) {
    let control = server.step_service.control;
    let status = server.step_service.status;

    let reason = loop {
        match conn.next().await {
            GattConnectionEvent::Disconnected { reason } => break reason,
            GattConnectionEvent::Gatt { event } => {
                let reply = match event {
                    GattEvent::Read(event) if event.handle() == status.handle => {
                        let (_, synced) = steps::now();
                        let value = PodStatus::new(steps::pending(), synced).to_bytes();
                        if let Err(e) = conn.set(&status, &value) {
                            warn!("[gatt] failed to update status: {:?}", e);
                        }
                        event.accept()
                    }
                    GattEvent::Write(event) if event.handle() == control.handle => {
                        let result = event.with_data(|offset, data| {
                            if offset != 0 {
                                Err(AttErrorCode::INVALID_OFFSET)
                            } else {
                                handle_control(data)
                            }
                        });
                        match result {
                            Ok(()) => event.accept(),
                            Err(code) => event.reject(code),
                        }
                    }
                    other => other.accept(),
                };
                match reply {
                    Ok(reply) => reply.send().await,
                    Err(e) => warn!("[gatt] error sending response: {:?}", e),
                }
            }
            _ => {}
        }
    };
    info!("[gatt] disconnected: {:?}", reason);
}

fn handle_control(data: &[u8]) -> Result<(), AttErrorCode> {
    match ControlCommand::parse(data) {
        Ok(ControlCommand::SetTime { utc_s }) => {
            steps::set_utc(utc_s);
            info!("[ctrl] time set to {}", utc_s);
            Ok(())
        }
        Ok(ControlCommand::Ack { seq }) => {
            // TODO(comm): delete records up to `seq` from flash once storage exists.
            info!("[ctrl] app acked up to seq {}", seq);
            Ok(())
        }
        Err(ControlError::BadLength) => Err(AttErrorCode::INVALID_ATTRIBUTE_VALUE_LENGTH),
        Err(ControlError::UnknownOpcode) => Err(AttErrorCode::REQUEST_NOT_SUPPORTED),
    }
}

/// Once the app subscribes, drain the buffer and then stream live steps.
async fn stream_steps_task<P: PacketPool>(server: &Server<'_>, conn: &GattConnection<'_, '_, P>) {
    let record = server.step_service.record;
    loop {
        // TODO(comm): wake on the CCCD write instead of polling.
        if !record.should_notify(conn) {
            Timer::after_millis(200).await;
            continue;
        }
        let rec = steps::next().await;
        if !record.should_notify(conn) {
            continue; // unsubscribed while waiting; rec stays in flight
        }
        match record.notify(conn, &rec.to_bytes(), false).await {
            Ok(()) => steps::sent(),
            Err(e) => {
                warn!("[notify] failed on seq {}: {:?}", rec.seq, e);
                return;
            }
        }
    }
}
