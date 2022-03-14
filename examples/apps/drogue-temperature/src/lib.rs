/*
 * MIT License
 *
 * Copyright (c) [2022] [Ondrej Babec <ond.babec@gmail.com>]
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to deal
 * in the Software without restriction, including without limitation the rights
 * to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
 * copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in all
 * copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
 * SOFTWARE.
 */

#![no_std]
#![macro_use]
#![feature(type_alias_impl_trait)]
#![feature(generic_associated_types)]
use core::future::Future;
use heapless::String;
use drogue_device::{
    actors::sensors::Temperature,
    actors::tcp::TcpActor,
    domain::{
        temperature::{Celsius, TemperatureScale},
        SensorAcquisition,
    },
    drivers::dns::*,
    drogue,
    traits::button::Button,
    traits::{ip::*, sensors::temperature::TemperatureSensor},
    Actor, ActorContext, Address, Inbox, Package,
};
use embassy::executor::Spawner;
use embedded_hal::digital::v2::InputPin;
use embedded_hal_async::digital::Wait;
use serde::{Deserialize, Serialize};

use core::convert::Infallible;
use defmt::{error, info, warn, trace};
use drogue_network::drogue_network::{DrogueNetwork, DrogueConnectionFactory};
use drogue_device::traits::dns::DnsResolver;
use rust_mqtt::client::client_config::ClientConfig;
use rust_mqtt::client::client_v5::MqttClientV5;
use rust_mqtt::network::network_trait::{NetworkConnection, NetworkConnectionFactory};
use rust_mqtt::packet::v5::publish_packet::QualityOfService;

#[derive(Clone, Serialize, Deserialize, Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct GeoLocation {
    pub lon: f32,
    pub lat: f32,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct TemperatureData {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub geoloc: Option<GeoLocation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temp: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hum: Option<f32>,
}

pub enum Command {
    Update(TemperatureData),
    Send,
}

pub struct App<B>
    where
        B: TemperatureBoard + 'static,
{
    host: &'static str,
    port: u16,
    username: &'static str,
    password: &'static str,
    connection_factory: DrogueConnectionFactory<<B as TemperatureBoard>::Network>,
}

impl<B> App<B>
    where
        B: TemperatureBoard + 'static,
{
    pub fn new(
        host: &'static str,
        port: u16,
        username: &'static str,
        password: &'static str,
        address: Address<<B as TemperatureBoard>::Network>
    ) -> Self {
        Self {
            host,
            port,
            username,
            password,
            connection_factory: DrogueConnectionFactory::new(address),
        }
    }
}

impl<B> Actor for App<B>
    where
        B: TemperatureBoard + 'static,
{
    type Message<'m>
        where
            B: 'm,
    = Command;

    type OnMountFuture<'m, M>
        where
            B: 'm,
            M: 'm,
    = impl Future<Output = ()> + 'm;
    fn on_mount<'m, M>(
        &'m mut self,
        _: Address<Self>,
        inbox: &'m mut M,
    ) -> Self::OnMountFuture<'m, M>
        where
            M: Inbox<Self> + 'm,
    {
        async move {
            let mut counter: usize = 0;
            let mut data: Option<TemperatureData> = None;
            let mut config = ClientConfig::<5>::new();
            config.qos = QualityOfService::QoS0;
            config.keep_alive = 360;
            let mut rec_buf = [0; 1024];
            let mut send_buf = [0; 1024];
            let topic = "test/topic";

            loop {
                match inbox.next().await {
                    Some(mut m) => match m.message() {
                        Command::Update(d) => {
                            data.replace(d.clone());
                        }
                        Command::Send => {
                            if let Some(sensor_data) = data.as_ref() {
                                info!("Sending temperature measurement number {}", counter);
                                counter += 1;

                                let network = self.connection_factory.connect([37, 205, 11, 180], 1997).await;
                                if let Ok(connection) = network {
                                    trace!("Connected");
                                    let mut client = MqttClientV5::<DrogueNetwork<<B as TemperatureBoard>::Network>, 5>::new(
                                        connection,
                                        & mut send_buf,
                                        1024,
                                        & mut rec_buf,
                                        1024,
                                        config.clone());

                                    trace!("Connection to broker!");
                                    let mut result = { client.connect_to_broker().await };
                                    trace!("Connected to broker!");

                                    let msg: String<128> = serde_json_core::ser::to_string(&sensor_data).unwrap();
                                    info!("[Publisher] Sending new message {} to topic {}", msg, topic);
                                    result = { client.send_message(topic, msg.as_str()).await };
                                    trace!("Message sent");

                                    info!("[Publisher] Disconnecting!");
                                    result = { client.disconnect().await };
                                    match result {
                                        Ok(result) => {
                                            info!("Everything went smooth")
                                        }
                                        Err(e) => {
                                            warn!("Error doing MQTT request");
                                        }
                                    }
                                } else {
                                    info!("Not temperature measurement received yet");
                                }
                            }
                        }
                    },
                    _ => {}
                }
            }
        }
    }
}

static DNS: StaticDnsResolver<'static, 2> = StaticDnsResolver::new(&[
    DnsEntry::new("localhost", IpAddress::new_v4(127, 0, 0, 1)),
    DnsEntry::new(
        "http.sandbox.drogue.cloud",
        IpAddress::new_v4(10, 200, 153, 9),
    ),
]);

pub trait TemperatureBoard {
    type NetworkPackage: Package<Primary = Self::Network>;
    type Network: TcpActor;
    type TemperatureScale: TemperatureScale;
    type Sensor: TemperatureSensor<Self::TemperatureScale>;
    type SensorReadyIndicator: Wait + InputPin;
    type SendTrigger: SendTrigger;
    type Rng: rand_core::RngCore + rand_core::CryptoRng;
}

pub trait SendTrigger {
    type TriggerFuture<'m>: Future
    where
    Self: 'm;
    fn wait<'m>(&'m mut self) -> Self::TriggerFuture<'m>;
}

pub struct TemperatureDevice<B>
    where
        B: TemperatureBoard + 'static,
{
    network: B::NetworkPackage,
    app: ActorContext<App<B>, 3>,
    trigger: ActorContext<AppTrigger<B>>,
    sensor: ActorContext<Temperature<B::SensorReadyIndicator, B::Sensor, B::TemperatureScale>>,
}

pub struct TemperatureBoardConfig<B>
    where
        B: TemperatureBoard + 'static,
{
    pub sensor: B::Sensor,
    pub sensor_ready: B::SensorReadyIndicator,
    pub send_trigger: B::SendTrigger,
    pub network_config: <B::NetworkPackage as Package>::Configuration,
}

impl<B> TemperatureDevice<B>
    where
        B: TemperatureBoard + 'static,
{
    pub fn new(network: B::NetworkPackage) -> Self {
        Self {
            network,
            sensor: ActorContext::new(),
            trigger: ActorContext::new(),
            app: ActorContext::new(),
        }
    }

    pub async fn mount(
        &'static self,
        spawner: Spawner,
        _rng: B::Rng,
        config: TemperatureBoardConfig<B>,
    ) {
        let network = self.network.mount(config.network_config, spawner);

        let app = self.app.mount(
            spawner,
            App::new(
                HOST,
                PORT.parse::<u16>().unwrap(),
                USERNAME.trim_end(),
                PASSWORD.trim_end(),
                network,
            ),
        );
        let sensor = self.sensor.mount(
            spawner,
            Temperature::new(config.sensor_ready, config.sensor),
        );
        self.trigger.mount(
            spawner,
            AppTrigger::new(config.send_trigger, sensor, app.into()),
        );
    }
}

pub struct AppTrigger<B>
    where
        B: TemperatureBoard + 'static,
{
    trigger: B::SendTrigger,
    sensor: Address<Temperature<B::SensorReadyIndicator, B::Sensor, B::TemperatureScale>>,
    app: Address<App<B>>,
}

impl<B> AppTrigger<B>
    where
        B: TemperatureBoard + 'static,
{
    pub fn new(
        trigger: B::SendTrigger,
        sensor: Address<Temperature<B::SensorReadyIndicator, B::Sensor, B::TemperatureScale>>,
        app: Address<App<B>>,
    ) -> Self {
        Self {
            trigger,
            sensor,
            app,
        }
    }
}

impl<B> Actor for AppTrigger<B>
    where
        B: TemperatureBoard + 'static,
{
    type OnMountFuture<'m, M>
        where
            Self: 'm,
            B: 'm,
            M: 'm,
    = impl Future<Output = ()> + 'm;

    fn on_mount<'m, M>(&'m mut self, _: Address<Self>, _: &'m mut M) -> Self::OnMountFuture<'m, M>
        where
            M: Inbox<Self> + 'm,
    {
        async move {
            loop {
                self.trigger.wait().await;
                trace!("Trigger activated! Requesting sensor data");
                if let Ok(data) = self.sensor.temperature().await {
                    let data = TemperatureData {
                        geoloc: None,
                        temp: Some(data.temperature.raw_value()),
                        hum: Some(data.relative_humidity),
                    };
                    trace!("Updating temperature data: {:?}", data);
                    let _ = self.app.request(Command::Update(data)).unwrap().await;
                }
                let _ = self.app.request(Command::Send).unwrap().await;
            }
        }
    }
}

impl<B> SendTrigger for B
    where
        B: Button + 'static,
{
    type TriggerFuture<'m>
        where
            B: 'm,
    = impl Future + 'm;
    fn wait<'m>(&'m mut self) -> Self::TriggerFuture<'m> {
        self.wait_released()
    }
}

pub struct TimeTrigger(pub embassy::time::Duration);
impl SendTrigger for TimeTrigger {
    type TriggerFuture<'m>
        where
            Self: 'm,
    = impl Future + 'm;
    fn wait<'m>(&'m mut self) -> Self::TriggerFuture<'m> {
        embassy::time::Timer::after(self.0)
    }
}


pub struct AlwaysReady;
impl embedded_hal_1::digital::ErrorType for AlwaysReady {
    type Error = Infallible;
}

impl Wait for AlwaysReady {
    type WaitForHighFuture<'a>
        where
            Self: 'a,
    = impl Future<Output = Result<(), Self::Error>> + 'a;

    fn wait_for_high<'a>(&'a mut self) -> Self::WaitForHighFuture<'a> {
        async move { Ok(()) }
    }

    type WaitForLowFuture<'a>
        where
            Self: 'a,
    = impl Future<Output = Result<(), Self::Error>> + 'a;

    fn wait_for_low<'a>(&'a mut self) -> Self::WaitForLowFuture<'a> {
        async move { Ok(()) }
    }

    type WaitForRisingEdgeFuture<'a>
        where
            Self: 'a,
    = impl Future<Output = Result<(), Self::Error>> + 'a;

    fn wait_for_rising_edge<'a>(&'a mut self) -> Self::WaitForRisingEdgeFuture<'a> {
        async move { Ok(()) }
    }

    type WaitForFallingEdgeFuture<'a>
        where
            Self: 'a,
    = impl Future<Output = Result<(), Self::Error>> + 'a;

    fn wait_for_falling_edge<'a>(&'a mut self) -> Self::WaitForFallingEdgeFuture<'a> {
        async move { Ok(()) }
    }

    type WaitForAnyEdgeFuture<'a>
        where
            Self: 'a,
    = impl Future<Output = Result<(), Self::Error>> + 'a;

    fn wait_for_any_edge<'a>(&'a mut self) -> Self::WaitForAnyEdgeFuture<'a> {
        async move { Ok(()) }
    }
}

impl InputPin for AlwaysReady {
    type Error = ();
    fn is_high(&self) -> Result<bool, Self::Error> {
        Ok(true)
    }

    fn is_low(&self) -> Result<bool, Self::Error> {
        Ok(false)
    }
}

pub struct FakeSensor(pub f32);

impl TemperatureSensor<Celsius> for FakeSensor {
    type Error = ();
    type CalibrateFuture<'m> = impl Future<Output = Result<(), Self::Error>> + 'm;
    fn calibrate<'m>(&'m mut self) -> Self::CalibrateFuture<'m> {
        async move { Ok(()) }
    }

    type ReadFuture<'m> =
    impl Future<Output = Result<SensorAcquisition<Celsius>, Self::Error>> + 'm;
    fn temperature<'m>(&'m mut self) -> Self::ReadFuture<'m> {
        async move {
            Ok(SensorAcquisition {
                relative_humidity: 0.0,
                temperature: drogue_device::domain::temperature::Temperature::new(self.0),
            })
        }
    }
}


const HOST: &str = drogue::config!("hostname");
const PORT: &str = drogue::config!("port");
const USERNAME: &str = drogue::config!("mqtt-username");
const PASSWORD: &str = drogue::config!("mqtt-password");