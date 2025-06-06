use std::time::Duration;

use async_hid::{AsyncHidRead, HidBackend, HidResult};
use async_io::Timer;
use futures_lite::{FutureExt, StreamExt};
use simple_logger::SimpleLogger;

#[pollster::main]
async fn main() -> HidResult<()> {
    SimpleLogger::new().init().unwrap();

    let mut device = HidBackend::default()
        .enumerate()
        .await?
        .find(|info| info.matches(0xFF00, 0x1, 0x1038, 0x2206))
        .await
        .inspect(|info| {
            println!(
                "{}: 0x{:X} 0x{:X} 0x{:X} 0x{:X} {:?}",
                info.name, info.usage_page, info.usage_id, info.vendor_id, info.product_id, info.id
            );
        })
        .expect("Could not find device")
        .open_readable()
        .await?;

    let mut buffer = [0u8; 8];
    loop {
        let size = device
            .read_input_report(&mut buffer)
            .or(async {
                Timer::after(Duration::from_secs(4)).await;
                Ok(0)
            })
            .await?;
        //sleep(std::time::Duration::from_millis(10));
        println!("{:?}", &buffer[..size]);
    }
}
