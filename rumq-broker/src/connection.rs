use derive_more::From;
use rumq_core::{connack, MqttRead, MqttWrite, Packet, Connect, ConnectReturnCode};
use tokio::sync::mpsc::{channel, Receiver};
use tokio::sync::mpsc::error::SendError;
use tokio::stream::StreamExt;
use tokio::time;
use tokio::select;

use crate::router::{self, RouterMessage};
use crate::Network;
use crate::ServerSettings;

use std::sync::Arc;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

#[derive(Debug, From)]
pub enum Error {
    Core(rumq_core::Error),
    Timeout(time::Elapsed),
    Router(router::Error),
    Send(SendError<(String, RouterMessage)>),
    /// Received a wrong packet while waiting for another packet
    WrongPacket,
    /// Invalid client ID
    InvalidClientId,
}

pub async fn eventloop(config: Arc<ServerSettings>, router: Rc<RefCell<router::Router>>, stream: impl Network) -> Result<String, Error> {
    let mut connection = Connection::new(config, stream, router.clone()).await?;
    let id = connection.id.clone();

    if let Err(err) = connection.run().await {
        error!("Connection error = {:?}. Id = {}", err, id);
        router.borrow_mut().deactivate_and_forward_will(id.clone());
    }

    Ok(id)
}

pub struct Connection<S> {
    id:         String,
    keep_alive: Duration,
    stream:     S,
    this_rx:    Receiver<RouterMessage>,
    router: Rc<RefCell<router::Router>>,
}

impl<S: Network> Connection<S> {
    async fn new(config: Arc<ServerSettings>, mut stream: S, router: Rc<RefCell<router::Router>>) -> Result<Connection<S>, Error> {
        let (this_tx, this_rx) = channel(100);
        let timeout = Duration::from_millis(config.connection_timeout_ms.into());
        let router = router.clone();
        let connect = time::timeout(timeout, async {
            let packet = stream.mqtt_read().await?;
            let o = handle_incoming_connect(packet)?;
            Ok::<_, Error>(o)
        })
        .await??;


        let id = connect.client_id.clone();
        let keep_alive = Duration::from_secs(connect.keep_alive as u64);

        router.borrow_mut().handle_connect(connect, this_tx)?;
        let connack = connack(ConnectReturnCode::Accepted, true);
        let packet = Packet::Connack(connack);
        stream.mqtt_write(&packet).await?;
        let connection = Connection { id, keep_alive, stream, router, this_rx };
        Ok(connection)
    }

    async fn run(&mut self) -> Result<(), Error> {
        let stream = &mut self.stream;
        let id = self.id.to_owned();
        let router = self.router.clone();

        // eventloop which processes packets and router messages
        loop {
            let keep_alive = self.keep_alive + self.keep_alive.mul_f32(0.5);

            let id = id.clone();
            // TODO: Use Delay::reset to not construct this timeout future everytime
            let packet = time::timeout(keep_alive, async {
                let packet = stream.mqtt_read().await?;
                match packet {
                    Packet::Pingreq => return Ok((Some(Packet::Pingresp), None)),
                    _ => ()
                }
                let  reply = router.borrow_mut().handle_incoming_packet(&id.clone(), packet.clone())?;
                Ok::<_, Error>((reply, Some(packet)))
            });

            let this_rx = &mut self.this_rx;
            let message = async {
                this_rx.next().await
            };

            select! {
                // read packets from network and generate network reply and router message
                o = packet => {
                    match o?? {
                        (reply, packet) => {
                            if let Some(reply) = reply {
                                stream.mqtt_write(&reply).await?
                            }

                            if let Some(packet) = packet {
                                match packet {
                                    Packet::Publish(publish) => router.borrow_mut().match_subscriptions_and_forward(&id.clone(), publish),
                                    Packet::Subscribe(subscribe) => router.borrow_mut().add_to_subscriptions(id, subscribe),
                                    Packet::Unsubscribe(unsubscribe) => router.borrow_mut().remove_from_subscriptions(id, unsubscribe),
                                    Packet::Disconnect => router.borrow_mut().deactivate(id),
                                    _ => continue,
                                }
                            }
                        }
                    };
                }

                // read packets from the router and write to network
                // router can close the connection by dropping tx handle. this should stop this
                // eventloop without sending the death notification
                o = message => match o {
                    Some(message) => {
                        match message {
                            RouterMessage::Packet(packet) => stream.mqtt_write(&packet).await?,
                            _ => return Err(Error::WrongPacket)
                        }
                    }
                    None => {
                        info!("Tx closed!! Stopping the connection");
                        return Ok(())
                    }
                }
            };
        }
    }
}

pub fn handle_incoming_connect(packet: Packet) -> Result<Connect, Error> {
    let mut connect = match packet {
        Packet::Connect(connect) => connect,
        packet => {
            error!("Invalid packet. Expecting connect. Received = {:?}", packet);
            return Err(Error::WrongPacket);
        }
    };

    // this broker expects a keepalive. 0 keepalives are promoted to 10 minutes
    if connect.keep_alive == 0 {
        warn!("0 keepalive. Promoting it to 10 minutes");
        connect.keep_alive = 10 * 60;
    }

    if connect.client_id.starts_with(' ') || connect.client_id.is_empty() {
        error!("Client id shouldn't start with space (or) shouldn't be emptys");
        return Err(Error::InvalidClientId);
    }

    Ok(connect)
}
