#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use embassy_crypto_rustcrypto as _;
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_hal::clock::CpuClock;
use esp_hal::gpio::{Input, InputConfig, Pull};
use esp_hal::timer::timg::TimerGroup;
use esp_println as _;
use esp_radio::ble::controller::BleConnector;
use esp_storage::FlashStorage;
use trouble_host::prelude::*;

#[path = "../ble.rs"]
mod ble;
#[path = "../csprng.rs"]
mod csprng;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

extern crate alloc;

// const CONNECTIONS_MAX: usize = 1;
// const L2CAP_CHANNELS_MAX: usize = 1;

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    // generator version: 1.3.0
    // generator parameters: --chip esp32c6 -o unstable-hal -o alloc -o embassy -o ble-trouble
    esp_println::logger::init_logger(log::LevelFilter::Info);

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    let mut seed = [0u8; 32];
    esp_hal::rng::Rng::new().read(&mut seed);
    csprng::seed(seed);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 65536);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    let mut button = Input::new(
        peripherals.GPIO9, // GPIO0 on ESP32/S3
        InputConfig::default().with_pull(Pull::Up),
    );
    // find more examples https://github.com/embassy-rs/trouble/tree/main/examples/esp32
    let transport = BleConnector::new(peripherals.BT, Default::default()).unwrap();
    let ble_controller = ExternalController::<_, 1>::new(transport);

    let flash_storage = FlashStorage::new(peripherals.FLASH);
    use embedded_storage::nor_flash::NorFlash;
    let erase_size = <FlashStorage as NorFlash>::ERASE_SIZE as u32;
    let capacity = flash_storage.capacity() as u32;
    let storage_range = (capacity - erase_size * 2)..capacity;
    let mut flash = embassy_embedded_hal::adapter::BlockingAsync::new(flash_storage);

    //To start the bluetooth stack
    ble::run(ble_controller, &mut flash, storage_range, &mut button).await;

    loop {
        Timer::after(Duration::from_secs(1)).await;
    }
    // for inspiration have a look at the examples at https://github.com/esp-rs/esp-hal/tree/esp-hal-v1.1.0/examples
}
