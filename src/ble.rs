use embassy_futures::join::join;
use embassy_futures::select::select;
use esp_hal::gpio::Input;
use log::{info, warn};
use trouble_host::prelude::*; // Added this to fix the macro error

/// Max number of connections
const CONNECTIONS_MAX: usize = 1;

/// Max number of L2CAP channels.
const L2CAP_CHANNELS_MAX: usize = 3; // Signal + att

const REPORT_MAP: &[u8] = &[
    0x05, 0x01, //Usage Page (Generic Desktop)
    0x09, 0x06, //Usage (Keyboard)
    0xa1, 0x01, //Collection (Application)
    0x05, 0x07, //Usage Page (Keyboard)
    0x19, 0xe0, //Usage Minimum (Keyboard LeftControl)
    0x29, 0xe7, //Usage Maximum (Keyboard Right GUI)
    0x15, 0x00, //Logical Minimum (0)
    0x25, 0x01, //Logical Maximum (1)
    0x75, 0x01, //Report Size (1)
    0x95, 0x08, //Report Count (8)
    0x81, 0x02, //Input (Data, Variable, Absolute) Modifier byte
    0x95, 0x01, //Report Count (1)
    0x75, 0x08, //Report Size (8)
    0x81, 0x01, //Input (Constant) Reserved byte
    0x95, 0x06, //Report Count (6)
    0x75, 0x08, //Report Size (8)
    0x15, 0x00, //Logical Minimum (0)
    0x25, 0x65, //Logical Maximum (101)
    0x05, 0x07, //Usage Page (Key Codes)
    0x19, 0x00, //   Usage Minimum (0)
    0x29, 0x65, //   Usage Maximum (101)
    0x81, 0x00, //   Input (Data, Array) - 6 key slots
    0xc0, //End Collection
];

// GATT Server definition
#[gatt_server]
struct Server {
    hid_service: HidService,
}

#[gatt_service(uuid = service::HUMAN_INTERFACE_DEVICE)]
struct HidService {
    //0x03 means (remote wake 0x01 + 0x02 normally connectable)
    #[characteristic(uuid = "2A4A", read, value = [0x11, 0x01, 0x00, 0x03])]
    hid_info: [u8; 4],
    #[characteristic(uuid = "2A4B", read, value = REPORT_MAP)]
    report_map: &'static [u8],
    #[characteristic(uuid = "2A4C", write_without_response)]
    hid_control_point: u8,
    #[characteristic(uuid = "2A4D", read, notify)]
    #[descriptor(uuid = "2908", read, value = [0x00, 0x01])]
    input_report: [u8; 8],
    #[characteristic(uuid = "2A4E", read, write_without_response, value = 1)]
    protocol_mode: u8,
}

/// Run the BLE stack.
pub async fn run<C>(controller: C, mut button: &mut Input<'_>)
where
    C: Controller,
{
    // Using a fixed "random" address can be useful for testing. In real scenarios, one would
    // use e.g. the MAC 6 byte array as the address (how to get that varies by the platform).
    let address: Address = Address::random([0xff, 0x8f, 0x1a, 0x05, 0xe4, 0xee]);
    info!("Our address = {:?}", address);

    let mut resources: HostResources<DefaultPacketPool, CONNECTIONS_MAX, L2CAP_CHANNELS_MAX> =
        HostResources::new();
    let stack = trouble_host::new(controller, &mut resources)
        .set_random_address(address)
        .set_io_capabilities(IoCapabilities::NoInputNoOutput)
        .build();
    let runner = stack.runner();
    let mut peripheral = stack.peripheral();

    info!("Starting advertising and GATT service");
    let server = Server::new_with_config(GapConfig::Peripheral(PeripheralConfig {
        name: "KEEb",
        appearance: &appearance::human_interface_device::KEYBOARD,
    }))
    .unwrap();

    let _ = join(ble_task(runner), async {
        loop {
            match advertise("Keeb", &mut peripheral, &server).await {
                Ok(conn) => {
                    // set up tasks when the connection is established to a central, so they don't run when no one is connected.
                    conn.raw().set_bondable(true).unwrap();
                    let a = gatt_events_task(&server, &conn);
                    let b = custom_task(&server, &conn, &stack, &mut button);
                    // run until any task ends (usually because the connection has been closed),
                    // then return to advertising state.
                    select(a, b).await;
                    // a.await;
                }
                Err(e) => {
                    #[cfg(feature = "defmt")]
                    let e = defmt::Debug2Format(&e);
                    panic!("[adv] error: {:?}", e);
                }
            }
        }
    })
    .await;
}

async fn ble_task<C: Controller, P: PacketPool>(mut runner: Runner<'_, C, P>) {
    loop {
        if let Err(e) = runner.run().await {
            #[cfg(feature = "defmt")]
            let e = defmt::Debug2Format(&e);
            panic!("[ble_task] error: {:?}", e);
        }
    }
}

/// Stream Events until the connection closes.
///
/// This function will handle the GATT events and process them.
/// This is how we interact with read and write requests.
async fn gatt_events_task<P: PacketPool>(
    server: &Server<'_>,
    conn: &GattConnection<'_, '_, P>,
) -> Result<(), Error> {
    let reason = loop {
        match conn.next().await {
            GattConnectionEvent::Disconnected { reason } => break reason,
            GattConnectionEvent::PairingComplete {
                security_level,
                bond,
            } => {
                info!("Paired with security level: {:?}", security_level);
                if let Some(bond) = bond {
                    // Persist the bond information (see Bonding section)
                }
            }
            GattConnectionEvent::Gatt { event } => {
                info!("[GATT] event: {:?}", event);
                match event.accept() {
                    Ok(reply) => {
                        let _ = reply.send().await;
                    }
                    Err(e) => warn!("[gatt] error accepting event: {:?}", e),
                }
            }

            _ => {}
        }
    };
    info!("[gatt] disconnected");
    Ok(())
}

/// Create an advertiser to use to connect to a BLE Central, and wait for it to connect.
async fn advertise<'values, 'server, C: Controller>(
    name: &'values str,
    peripheral: &mut Peripheral<'values, C, DefaultPacketPool>,
    server: &'server Server<'values>,
) -> Result<GattConnection<'values, 'server, DefaultPacketPool>, BleHostError<C::Error>> {
    let mut advertiser_data = [0; 31];
    let len = AdStructure::encode_slice(
        &[
            AdStructure::Flags(LE_GENERAL_DISCOVERABLE | BR_EDR_NOT_SUPPORTED),
            AdStructure::CompleteServiceUuids16(&[[0x12, 0x18]]),
            AdStructure::CompleteLocalName(name.as_bytes()),
        ],
        &mut advertiser_data[..],
    )?;
    let advertiser = peripheral
        .advertise(
            &Default::default(),
            Advertisement::ConnectableScannableUndirected {
                adv_data: &advertiser_data[..len],
                scan_data: &[],
            },
        )
        .await?;
    info!("[adv] advertising");
    let conn = advertiser.accept().await?.with_attribute_server(server)?;
    info!("[adv] connection established");
    Ok(conn)
}

/// Example task to use the BLE notifier interface.
/// This task will notify the connected central of a counter value every 2 seconds.
/// It will also read the RSSI value every 2 seconds.
/// and will stop when the connection is closed by the central or an error occurs.
async fn custom_task<C: Controller, P: PacketPool>(
    server: &Server<'_>,
    conn: &GattConnection<'_, '_, P>,
    stack: &Stack<'_, C, P>,
    button: &mut Input<'_>,
) {
    let mut tick: u8 = 0;
    let input_rep = server.hid_service.input_report;
    let press_report: [u8; 8] = [0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00];

    let release_report: [u8; 8] = [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];

    loop {
        info!("BEFORE PRESS");
        button.wait_for_falling_edge().await;
        let r = input_rep.notify(conn, &press_report, false).await;

        info!("AFTER PRESS: {:?}", r);

        info!("BEFORE RELEASE");

        button.wait_for_rising_edge().await;
        let r = input_rep.notify(conn, &release_report, false).await;

        info!("AFTER RELEASE: {:?}", r);
    }
}
