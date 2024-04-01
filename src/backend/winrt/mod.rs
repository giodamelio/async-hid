mod utils;
mod win32;

use std::pin::Pin;
use std::sync::OnceLock;
use std::task::{Context, Poll};

use flume::{Receiver, TrySendError};
use futures_lite::{Stream, StreamExt};
use windows::core::{h, HSTRING};
use windows::Devices::Enumeration::{DeviceInformation, DeviceInformationCollection};
use windows::Devices::HumanInterfaceDevice::{HidDevice, HidInputReport, HidInputReportReceivedEventArgs};
use windows::Foundation::{EventRegistrationToken, TypedEventHandler};
use windows::Storage::FileAccessMode;

use crate::backend::winrt::utils::{IBufferExt, WinResultExt};
use crate::error::{ErrorSource, HidResult};
use crate::{ensure, AccessMode, DeviceInfo, HidError};

const DEVICE_SELECTOR: &HSTRING = h!(
    r#"System.Devices.InterfaceClassGuid:="{4D1E55B2-F16F-11CF-88CB-001111000030}" AND System.Devices.InterfaceEnabled:=System.StructuredQueryType.Boolean#True"#
);

pub async fn enumerate() -> HidResult<impl Stream<Item = DeviceInfo> + Unpin + Send> {
    //let devices = DeviceInformation::FindAllAsyncAqsFilter(DEVICE_SELECTOR)?
    //    .await?
    //    .into_iter()
    //    .map(get_device_information)
    //    .collect::<FuturesUnordered<_>>()
    //    .filter_map(|info| ready(info.ok()))
    //    .collect()
    //    .await;
    let devices = DeviceInformation::FindAllAsyncAqsFilter(DEVICE_SELECTOR)?.await?;
    let devices = DeviceInformationSteam::from(devices)
        .then(|info| Box::pin(get_device_information(info)))
        .filter_map(|r| {
            r.map_err(|e| log::trace!("Failed to query device information\n\tbecause {e:?}"))
                .ok()
        });
    //.collect()
    //.await;
    Ok(devices)
}

//fn get_device_information_unpin(device: DeviceInformation) -> impl Future<Output = HidResult<DeviceInfo>> + Unpin {
//
//}

async fn get_device_information(device: DeviceInformation) -> HidResult<DeviceInfo> {
    let id = device.Id()?;
    let name = device.Name()?.to_string_lossy();
    let device = HidDevice::FromIdAsync(&id, FileAccessMode::Read)?;
    let device = device
        .await
        .on_null_result(|| HidError::custom(format!("Failed to open {name} (Id: {id})")))?;
    Ok(DeviceInfo {
        id: id.into(),
        name,
        product_id: device.ProductId()?,
        vendor_id: device.VendorId()?,
        usage_id: device.UsageId()?,
        usage_page: device.UsagePage()?,
        private_data: BackendPrivateData::default(),
    })
}

#[derive(Debug, Clone)]
struct InputReceiver {
    buffer: Receiver<HidInputReport>,
    token: EventRegistrationToken,
}

impl InputReceiver {
    fn new(device: &HidDevice) -> HidResult<Self> {
        let (sender, receiver) = flume::bounded(64);
        let drain = receiver.clone();
        let token = device.InputReportReceived(&TypedEventHandler::new(move |_, args: &Option<HidInputReportReceivedEventArgs>| {
            if let Some(args) = args {
                let mut msg = args.Report()?;
                while let Err(TrySendError::Full(ret)) = sender.try_send(msg) {
                    log::trace!("Dropping previous input report because the queue is full");
                    let _ = drain.try_recv();
                    msg = ret;
                }
            }
            Ok(())
        }))?;
        Ok(Self { buffer: receiver, token })
    }

    async fn recv_async(&self) -> HidInputReport {
        self.buffer
            .recv_async()
            .await
            .expect("Input report handler got dropped unexpectedly")
    }

    fn stop(self, device: &HidDevice) -> HidResult<()> {
        Ok(device.RemoveInputReportReceived(self.token)?)
    }
}

#[derive(Debug, Clone)]
pub struct BackendDevice {
    device: HidDevice,
    input: Option<InputReceiver>,
}

impl Drop for BackendDevice {
    fn drop(&mut self) {
        if let Some(input) = self.input.take() {
            input
                .stop(&self.device)
                .unwrap_or_else(|err| log::warn!("Failed to unregister input report callback\n\t{err:?}"));
        }
    }
}

pub async fn open(id: &BackendDeviceId, mode: AccessMode) -> HidResult<BackendDevice> {
    let device = HidDevice::FromIdAsync(id, mode.into())?
        .await
        .on_null_result(|| HidError::custom(format!("Failed to open {}", id)))?;
    let input = match mode.readable() {
        true => Some(InputReceiver::new(&device)?),
        false => None,
    };
    Ok(BackendDevice { device, input })
}

impl BackendDevice {
    pub async fn read_input_report(&self, buf: &mut [u8]) -> HidResult<usize> {
        let report = self
            .input
            .as_ref()
            .expect("Reading is disabled")
            .recv_async()
            .await;
        let buffer = report.Data()?;
        let buffer = buffer.as_slice()?;
        ensure!(!buffer.is_empty(), HidError::custom("Input report is empty"));
        let size = buf.len().min(buffer.len());
        let start = if buffer[0] == 0x0 { 1 } else { 0 };
        buf[..(size - start)].copy_from_slice(&buffer[start..size]);

        Ok(size - start)
    }

    pub async fn write_output_report(&self, buf: &[u8]) -> HidResult<()> {
        ensure!(!buf.is_empty(), HidError::zero_sized_data());
        let report = self.device.CreateOutputReport()?;

        {
            let mut buffer = report.Data()?;
            ensure!(buffer.Length()? as usize >= buf.len(), HidError::custom("Output report is too large"));
            let (buffer, remainder) = buffer.as_mut_slice()?.split_at_mut(buf.len());
            buffer.copy_from_slice(buf);
            remainder.fill(0);
        }

        self.device.SendOutputReportAsync(&report)?.await?;
        Ok(())
    }
}

#[derive(Default, Debug, Clone, Eq, PartialEq)]
pub struct BackendPrivateData {
    serial_number: OnceLock<Option<String>>,
}

pub type BackendDeviceId = HSTRING;
pub type BackendError = windows::core::Error;

impl From<BackendError> for ErrorSource {
    fn from(value: BackendError) -> Self {
        ErrorSource::PlatformSpecific(value)
    }
}

impl From<AccessMode> for FileAccessMode {
    fn from(value: AccessMode) -> Self {
        match value {
            AccessMode::Read => FileAccessMode::Read,
            AccessMode::Write => FileAccessMode::ReadWrite,
            AccessMode::ReadWrite => FileAccessMode::ReadWrite,
        }
    }
}

struct DeviceInformationSteam {
    devices: DeviceInformationCollection,
    index: u32,
}

impl From<DeviceInformationCollection> for DeviceInformationSteam {
    fn from(value: DeviceInformationCollection) -> Self {
        Self { devices: value, index: 0 }
    }
}

impl Stream for DeviceInformationSteam {
    type Item = DeviceInformation;

    fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let current = self.index;
        self.index += 1;
        Poll::Ready(self.devices.GetAt(current).ok())
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = (self
            .devices
            .Size()
            .expect("Failed to get the length of the collection")
            - self.index) as usize;
        (remaining, Some(remaining))
    }
}
