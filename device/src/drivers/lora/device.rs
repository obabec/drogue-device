use crate::traits::lora::{LoraError as DriverError, *};
use core::future::Future;
use embassy::time::*;

use lorawan_device::{
    radio, region, Device as LorawanDevice, Error as LorawanError, Event as LorawanEvent,
    Response as LorawanResponse, Timings,
};
use lorawan_encoding::default_crypto::DefaultFactory as Crypto;

#[derive(Debug, Clone)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum RadioPhyEvent {
    Irq,
}

pub trait RadioIrq {
    type Future<'m>: Future<Output = ()> + 'm;
    fn wait<'m>(&'m mut self) -> Self::Future<'m>;
}

pub trait Radio: radio::PhyRxTx<PhyEvent = RadioPhyEvent> + Timings {
    fn reset(&mut self) -> Result<(), DriverError>;
}

enum DriverState<'a, R>
where
    R: Radio + 'a,
{
    New(R, &'a mut [u8]),
    Configured(LorawanDevice<'a, R, Crypto>),
}

pub struct LoraDevice<'a, R, I>
where
    R: Radio + 'a,
    I: RadioIrq + 'a,
{
    state: Option<DriverState<'a, R>>,
    irq: I,
    get_random: fn() -> u32,
}

pub enum DriverEvent {
    ProcessAfter(u32),
    JoinSuccess,
    JoinFailed,
    SessionExpired,
    Ack,
    AckWithData(usize, [u8; 255]),
    AckTimeout,
    None,
}

impl<'a, R, I> LoraDevice<'a, R, I>
where
    R: Radio + 'a,
    I: RadioIrq + 'a,
{
    pub fn new(radio: R, irq: I, get_random: fn() -> u32, radio_tx_buf: &'a mut [u8]) -> Self {
        Self {
            irq,
            state: Some(DriverState::New(radio, radio_tx_buf)),
            get_random,
        }
    }

    fn process_event(&mut self, event: LorawanEvent<'a, R>) -> DriverEvent {
        //crate::log_stack("Process event");
        match self.state.take().unwrap() {
            DriverState::Configured(lorawan) => {
                let mut is_tx = false;
                let mut is_rx = false;
                let mut is_timeout = false;
                match &event {
                    LorawanEvent::NewSessionRequest => {
                        trace!("New Session Request");
                    }
                    LorawanEvent::RadioEvent(e) => match e {
                        radio::Event::TxRequest(_, _) => {
                            is_tx = true;
                            ()
                        }
                        radio::Event::RxRequest(_) => {
                            is_rx = true;
                            ()
                        }
                        radio::Event::CancelRx => (),
                        radio::Event::PhyEvent(_) => {
                            trace!("Phy event");
                        }
                    },
                    LorawanEvent::TimeoutFired => {
                        is_timeout = true;
                        ()
                    }
                    LorawanEvent::SendDataRequest(_e) => {
                        trace!("SendData");
                    }
                }
                let (mut new_state, response) = lorawan.handle_event(event);
                trace!("Event handled {} {} {}", is_rx, is_tx, is_timeout);
                let event = self.process_response(&mut new_state, response, is_timeout);
                self.state.replace(DriverState::Configured(new_state));
                event
            }
            s => {
                trace!("Not yet configured, event processing skipped");
                self.state.replace(s);
                DriverEvent::None
            }
        }
    }

    fn process_response<'m>(
        &self,
        lorawan: &mut LorawanDevice<'m, R, Crypto>,
        response: Result<LorawanResponse, LorawanError<R>>,
        is_rx: bool,
    ) -> DriverEvent
    where
        R: 'm,
    {
        //crate::log_stack("Process response");
        match response {
            Ok(response) => match response {
                LorawanResponse::TimeoutRequest(ms) => {
                    //let ms = if is_rx { ms - RX_DELAY1 } else { ms };

                    trace!("TimeoutRequest: {:?}", ms);
                    return DriverEvent::ProcessAfter(ms);
                }
                LorawanResponse::JoinSuccess => {
                    trace!("Joined successfully!");
                    return DriverEvent::JoinSuccess;
                }
                LorawanResponse::ReadyToSend => {
                    trace!("RxWindow expired but no ACK expected. Ready to Send");
                }
                LorawanResponse::DownlinkReceived(fcnt_down) => {
                    if let Some(downlink) = lorawan.take_data_downlink() {
                        use lorawan_encoding::parser::FRMPayload;

                        if let Ok(FRMPayload::Data(data)) = downlink.frm_payload() {
                            trace!(
                                "Downlink received \t\t(FCntDown={}\tFRM: {:?})",
                                fcnt_down,
                                data,
                            );
                            let mut buf = [0; 255];
                            buf[0..data.len()].copy_from_slice(&data[0..data.len()]);
                            return DriverEvent::AckWithData(data.len(), buf);
                        } else {
                            trace!("Downlink received \t\t(FcntDown={})", fcnt_down);
                            return DriverEvent::Ack;
                        }

                        /*
                        let fhdr = downlink.fhdr();
                        let fopts = fhdr.fopts();
                        let mut mac_commands_len = 0;
                        for mac_command in fopts {
                            if mac_commands_len == 0 {
                                trace!("\tFOpts: ");
                            }
                            trace!("{:?},", mac_command);
                            mac_commands_len += 1;
                        }
                        */
                    }
                }
                LorawanResponse::NoAck => {
                    trace!("RxWindow expired, expected ACK to confirmed uplink not received");
                    return DriverEvent::AckTimeout;
                }
                LorawanResponse::NoJoinAccept => {
                    trace!("No Join Accept Received. Retrying.");
                    return DriverEvent::JoinFailed;
                }
                LorawanResponse::SessionExpired => {
                    trace!("SessionExpired. Created new Session");
                    return DriverEvent::SessionExpired;
                }
                LorawanResponse::NoUpdate => {
                    info!("No update");
                    return DriverEvent::JoinFailed;
                }
                LorawanResponse::UplinkSending(fcnt_up) => {
                    trace!("Uplink with FCnt {}", fcnt_up);
                }
                LorawanResponse::JoinRequestSending => {
                    trace!("Join Request Sending");
                }
            },
            Err(err) => match err {
                LorawanError::Radio(_) => error!("Radio error"),
                LorawanError::Session(_) => error!("Session error"), //{:?}", e),
                LorawanError::NoSession(_) => error!("NoSession error"),
            },
        }
        DriverEvent::None
    }

    async fn join(&mut self) -> Result<(), LoraError> {
        //crate::log_stack("Driver join");
        let mut event: DriverEvent = self.process_event(LorawanEvent::NewSessionRequest);
        loop {
            match event {
                DriverEvent::ProcessAfter(ms) => {
                    let interrupt = self.irq.wait();
                    match with_timeout(Duration::from_millis(ms.into()), interrupt).await {
                        Ok(_) => {
                            event = self.process_event(LorawanEvent::RadioEvent(
                                radio::Event::PhyEvent(RadioPhyEvent::Irq),
                            ));
                        }
                        Err(TimeoutError) => {
                            event = self.process_event(LorawanEvent::TimeoutFired);
                        }
                    }
                }
                DriverEvent::JoinSuccess => {
                    trace!("Joined successfully");
                    return Ok(());
                }
                DriverEvent::JoinFailed => {
                    event = self.process_event(LorawanEvent::NewSessionRequest);
                }
                _ => {
                    // Wait for interrupt
                    self.irq.wait().await;
                    event = self.process_event(LorawanEvent::RadioEvent(radio::Event::PhyEvent(
                        RadioPhyEvent::Irq,
                    )));
                }
            }
        }
    }

    async fn send_data(
        &mut self,
        qos: QoS,
        port: Port,
        data: &[u8],
    ) -> Result<DriverEvent, LoraError> {
        match self.state.take().unwrap() {
            DriverState::Configured(lorawan) => {
                let ready_to_send = lorawan.ready_to_send_data();
                if ready_to_send {
                    let (mut new_state, response) = lorawan.send(
                        data,
                        port,
                        match qos {
                            QoS::Confirmed => true,
                            QoS::Unconfirmed => false,
                        },
                    );
                    let event = self.process_response(&mut new_state, response, false);
                    self.state.replace(DriverState::Configured(new_state));
                    Ok(event)
                } else {
                    self.state.replace(DriverState::Configured(lorawan));
                    Err(LoraError::NotReady)
                }
            }
            other => {
                //info!("Driver not yet initialized, ignoring configuration");
                self.state.replace(other);
                Err(LoraError::OtherError)
            }
        }
    }

    async fn send_recv(
        &mut self,
        qos: QoS,
        port: Port,
        data: &[u8],
        rx: Option<&mut [u8]>,
    ) -> Result<usize, LoraError> {
        // Await response
        let mut event = self.send_data(qos, port, data).await?;
        loop {
            match event {
                DriverEvent::ProcessAfter(ms) => {
                    let interrupt = self.irq.wait();
                    match with_timeout(Duration::from_millis(ms.into()), interrupt).await {
                        Ok(_) => {
                            event = self.process_event(LorawanEvent::RadioEvent(
                                radio::Event::PhyEvent(RadioPhyEvent::Irq),
                            ));
                        }
                        Err(TimeoutError) => {
                            event = self.process_event(LorawanEvent::TimeoutFired);
                        }
                    }
                }
                DriverEvent::AckWithData(len, buf) => {
                    trace!("Received {} bytes of data", len);
                    if let Some(rx) = rx {
                        rx[0..len].copy_from_slice(&buf[0..len]);
                    }
                    return Ok(len);
                }
                DriverEvent::AckTimeout => {
                    trace!("Ack timed out!");
                    return Err(LoraError::AckTimeout);
                }
                DriverEvent::Ack => {
                    trace!("Ack received!");
                    return Ok(0);
                }
                _ => {
                    // Wait for interrupt
                    self.irq.wait().await;
                    event = self.process_event(LorawanEvent::RadioEvent(radio::Event::PhyEvent(
                        RadioPhyEvent::Irq,
                    )));
                }
            }
        }
    }
}

const RX_DELAY1: u32 = 5000;

impl<'a, R, I> LoraDriver for LoraDevice<'a, R, I>
where
    R: Radio,
    I: RadioIrq,
{
    #[rustfmt::skip]
    type ConfigureFuture<'m> where 'a: 'm, R: 'm  = impl Future<Output = Result<(), LoraError>> + 'm;
    fn configure<'m>(&'m mut self, config: &'m LoraConfig) -> Self::ConfigureFuture<'m> {
        async move {
            match self.state.take().unwrap() {
                DriverState::New(mut radio, radio_tx_buf) => {
                    //crate::log_stack("lora driver configure");
                    radio.reset()?;
                    trace!("Configuring radio");
                    let dev_eui = config.device_eui.as_ref().expect("device EUI must be set");
                    let app_eui = config.app_eui.as_ref().expect("app EUI must be set");
                    let app_key = config.app_key.as_ref().expect("app KEY must be set");
                    //info!("Creating device");
                    let data_rate =
                        to_datarate(config.spreading_factor.unwrap_or(SpreadingFactor::SF7));
                    let region = to_region(config.region.unwrap_or(LoraRegion::EU868));
                    if let Err(e) = region {
                        return Err(e);
                    }
                    let mut region = region.unwrap();
                    region.set_receive_delay1(RX_DELAY1);
                    info!("Creating new DEVICE!");
                    let mut lorawan = LorawanDevice::new(
                        region,
                        radio,
                        dev_eui.reverse().into(),
                        app_eui.reverse().into(),
                        app_key.clone().into(),
                        self.get_random,
                        radio_tx_buf,
                    );
                    lorawan.set_datarate(data_rate);
                    self.state.replace(DriverState::Configured(lorawan));
                    Ok(())
                }
                other => {
                    //info!("Driver not yet initialized, ignoring configuration");
                    self.state.replace(other);
                    Err(LoraError::OtherError)
                }
            }
        }
    }

    #[rustfmt::skip]
    type JoinFuture<'m> where 'a: 'm, R: 'm  = impl Future<Output = Result<(), LoraError>> + 'm;
    fn join<'m>(&'m mut self, _: ConnectMode) -> Self::JoinFuture<'m> {
        async move { self.join().await }
    }

    #[rustfmt::skip]
    type SendFuture<'m> where 'a: 'm, R: 'm  = impl Future<Output = Result<(), LoraError>> + 'm;
    fn send<'m>(&'m mut self, qos: QoS, port: Port, data: &'m [u8]) -> Self::SendFuture<'m> {
        async move { self.send_recv(qos, port, data, None).await.map(|_| ()) }
    }

    #[rustfmt::skip]
    type SendRecvFuture<'m> where 'a: 'm, R: 'm = impl Future<Output = Result<usize, LoraError>> + 'm;
    fn send_recv<'m>(
        &'m mut self,
        qos: QoS,
        port: Port,
        data: &'m [u8],
        rx: &'m mut [u8],
    ) -> Self::SendRecvFuture<'m> {
        async move { self.send_recv(qos, port, data, Some(rx)).await }
    }
}

fn to_region(region: LoraRegion) -> Result<region::Configuration, LoraError> {
    match region {
        LoraRegion::EU868 => Ok(region::EU868::default().into()),
        LoraRegion::US915 => Ok(region::US915::default().into()),
        LoraRegion::CN470 => Ok(region::CN470::default().into()),
        _ => Err(LoraError::UnsupportedRegion),
    }
}

fn to_datarate(spreading_factor: SpreadingFactor) -> region::DR {
    match spreading_factor {
        SpreadingFactor::SF7 => region::DR::_5,
        SpreadingFactor::SF8 => region::DR::_4,
        SpreadingFactor::SF9 => region::DR::_3,
        SpreadingFactor::SF10 => region::DR::_2,
        SpreadingFactor::SF11 => region::DR::_1,
        SpreadingFactor::SF12 => region::DR::_0,
    }
}
