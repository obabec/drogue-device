#![no_std]
#![no_main]
#![macro_use]
#![feature(generic_associated_types)]
#![feature(type_alias_impl_trait)]

extern crate alloc;

use core::future::Future;
use defmt::{info, warn};
use defmt_rtt as _;
use panic_probe as _;

use drogue_device::{
    actors::socket::*,
    actors::wifi::*,
    bsp::{boards::nrf52::microbit::*, Board},
    clients::mqtt::*,
    domain::temperature::Celsius,
    drivers::dns::*,
    traits::dns::*,
    traits::ip::*,
    traits::tcp::*,
    *,
};
use drogue_device::{actors::wifi::esp8266::*, drogue, traits::wifi::*, DeviceContext, Package};
use embassy::util::Forever;
use embassy_nrf::{
    gpio::{Level, Output, OutputDrive},
    interrupt,
    peripherals::{P0_09, P0_10, UARTE0},
    uarte,
    uarte::{Uarte, UarteRx, UarteTx},
    Peripherals,
};
use rust_mqtt::{
    client::{client_config::ClientConfig, client_v5::MqttClientV5},
    packet::v5::{property::Property, publish_packet::QualityOfService},
};
use drogue_device::traits::button::Button;
use drogue_device::traits::led::TextDisplay;

const WIFI_SSID: &str = drogue::config!("wifi-ssid");
const WIFI_PSK: &str = drogue::config!("wifi-password");
const HOST: &str = drogue::config!("hostname");
const PORT: &str = drogue::config!("port");
const USERNAME: &str = drogue::config!("mqtt-username");
const PASSWORD: &str = drogue::config!("mqtt-password");
const TOPIC: &str = drogue::config!("mqtt-topic");
const TOPIC_S: &str = "topic/subs";

type TX = UarteTx<'static, UARTE0>;
type RX = UarteRx<'static, UARTE0>;
type ENABLE = Output<'static, P0_09>;
type RESET = Output<'static, P0_10>;

bind_bsp!(Microbit, BSP);

type WifiDriver = Esp8266Wifi<TX, RX, ENABLE, RESET>;
type WifiActor = <WifiDriver as Package>::Primary;


#[embassy::main]
async fn main(spawner: embassy::executor::Spawner, p: Peripherals) {
    let board = Microbit::new(p);

    static LED_MATRIX: ActorContext<LedMatrixActor> = ActorContext::new();
    let matrix = LED_MATRIX.mount(spawner, LedMatrixActor::new(board.led_matrix, None));

    let mut config = uarte::Config::default();
    config.parity = uarte::Parity::EXCLUDED;
    config.baudrate = uarte::Baudrate::BAUD115200;
    

    let irq = interrupt::take!(UARTE0_UART0);
    let uart = Uarte::new_with_rtscts(
        board.uarte0,
        irq,
        board.p0_13,
        board.p0_01,
        board.p0_03,
        board.p0_04,
        config,
    );

    let (tx, rx) = uart.split();

    let enable_pin = Output::new(board.p0_09, Level::Low, OutputDrive::Standard);
    let reset_pin = Output::new(board.p0_10, Level::Low, OutputDrive::Standard);


    static DRIVER: Forever<WifiDriver> = Forever::new();
    let driver = DRIVER.put(Esp8266Wifi::new(tx, rx, enable_pin, reset_pin));

    let mut wifi = driver.mount((), spawner);
    wifi.join(Join::Wpa {
        ssid: WIFI_SSID.trim_end(),
        password: WIFI_PSK.trim_end(),
    })
    .await
    .unwrap();

    let ips = DNS.resolve(HOST).await.expect("unable to resolve host");
    let ip = ips[0];
    let mut socket_publish = Socket::new(wifi, wifi.open().await.unwrap());

    socket_publish
        .connect(
            IpProtocol::Tcp,
            SocketAddress::new(ips[0], PORT.parse::<u16>().unwrap()),
        )
        .await
        .expect("Error creating connection");

    let mut socket_receiver = Socket::new(wifi, wifi.open().await.unwrap());

    socket_receiver
        .connect(
            IpProtocol::Tcp,
            SocketAddress::new(ips[0], PORT.parse::<u16>().unwrap()),
        )
        .await
        .expect("Error creating connection");

    static RECEIVER: ActorContext<Receiver> = ActorContext::new();
    let receiver = RECEIVER.mount(spawner, Receiver::new(matrix, DrogueNetwork::new(socket_receiver)));

    let mut config = ClientConfig::new();
    config.add_qos(QualityOfService::QoS1);
    config.max_packet_size = 60;
    config.keep_alive = 3600;
    config.properties.push(Property::ReceiveMaximum(20));
    let mut recv_buffer = [0; 100];
    let mut write_buffer = [0; 100];

    let mut client = MqttClientV5::<_, 5>::new(
        DrogueNetwork::new(socket_publish),
        &mut write_buffer,
        100,
        &mut recv_buffer,
        100,
        config,
    );
    client.connect_to_broker().await;
    let mut button = board.button_a;
    loop {
        defmt::info!("Application initialized. Press 'A' button to send data");
        button.wait_pressed().await;
        client.send_message(TOPIC, "Hello world!");
    }
}

static DNS: StaticDnsResolver<'static, 3> = StaticDnsResolver::new(&[
    DnsEntry::new("localhost", IpAddress::new_v4(127, 0, 0, 1)),
    DnsEntry::new(
        "http.sandbox.drogue.cloud",
        IpAddress::new_v4(65, 108, 135, 161),
    ),
    DnsEntry::new("obabec.eu", IpAddress::new_v4(37, 205, 11, 180)),
]);

pub struct Receiver {
    display: Address<LedMatrixActor>,
    socket: DrogueNetwork<WifiActor>,
}

impl Receiver {
    pub fn new(display: Address<LedMatrixActor>, socket: DrogueNetwork<WifiActor>) -> Self {
        Self {
            display,
            socket,
        }
    }
}

#[derive(Clone)]
pub enum ReceiverMessage {
    Toggle,
}

impl Actor for Receiver {
    type Message<'m> = ReceiverMessage;

    type OnMountFuture<'m, M> = impl Future<Output = ()> + 'm
    where
    M: 'm + Inbox<Self>,
    Self: 'm;
    fn on_mount<'m, M>(
        &'m mut self,
        _: Address<Self>,
        inbox: &'m mut M,
    ) -> Self::OnMountFuture<'m, M>
        where
            M: Inbox<Self> + 'm,
    {
        async move {
            let mut config = ClientConfig::new();
            config.add_qos(QualityOfService::QoS1);
            config.max_packet_size = 60;
            config.keep_alive = 3600;
            config.properties.push(Property::ReceiveMaximum(20));
            let mut recv_buffer = [0; 100];
            let mut write_buffer = [0; 100];

            let mut client = MqttClientV5::<_, 5>::new(
                self.socket,
                &mut write_buffer,
                100,
                &mut recv_buffer,
                100,
                config,
            );
            client.connect_to_broker().await;
            client.subscribe_to_topic(TOPIC_S).await;
            loop {
                let msg = { client.receive_message().await };
                if msg.is_ok() {
                    let message = msg.unwrap();
                    info!("Received!");
                    self.display.scroll(core::str::from_utf8(message).unwrap());
                } else {
                    warn!("Could not get message!");
                }

            }
        }
    }
}