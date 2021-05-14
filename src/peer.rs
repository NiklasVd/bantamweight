use std::{collections::HashMap, error::Error, io::ErrorKind, net::SocketAddr, sync::Arc};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::{TcpListener, TcpStream}, sync::{mpsc, Mutex}};
use crate::{AM, BantamPacketType, BinaryStream, ByePacket, DataPacket, HandshakePacket, HandshakeResponsePacket, PacketHeader, Serializable, SerializableSocketAddr};

type Tx = mpsc::UnboundedSender<Vec<u8>>;

pub struct Peer {
    // Add ingoing/outgoing peer tag?
    pub listener_addr: SocketAddr,
    sender: Tx
}

pub struct PeerSharedState {
    listener_addr: SocketAddr,
    peers: HashMap<SocketAddr, Peer>
}

impl PeerSharedState {
    pub fn get_peers(&self) -> Vec<(&SocketAddr, &Peer)> {
        let peers = self.peers.iter().map(|pair| {
            (pair.0, pair.1)
        }).collect();

        peers
    }

    fn add_peer(&mut self, addr: SocketAddr, peer: Peer) -> bool {
        self.peers.insert(addr, peer).is_none()
    }

    fn remove_peer(&mut self, addr: SocketAddr) -> bool {
        self.peers.remove(&addr).is_some()
    }

    async fn broadcast(&self, bytes: Vec<u8>) -> Result<(), Box<dyn Error + Send + Sync>> {
        Ok(for peer in self.peers.iter() {
            peer.1.sender.send(bytes.clone())? // Get rid of clone
        })
    }

    async fn unicast(&mut self, bytes: Vec<u8>, addr: SocketAddr) -> Result<(), Box<dyn Error + Send + Sync>> {
        // Improve error handling
        if let Some(peer) = self.peers.get(&addr) {
            peer.sender.send(bytes)?;
        }
        Ok(())
    }
}

// --- Called by a peer that wants to connect ot a P2P network. ---
pub async fn setup_peer<F: Fn(Vec<u8>, SocketAddr) + Send + Sync + 'static>(port: u16, addr: SocketAddr,
    on_receive_data_packet: Arc<F>) -> Result<AM<PeerSharedState>, Box<dyn Error + Send + Sync>> {
    let shared_state = setup_ingoing_peer(port, on_receive_data_packet.clone()).await?;
    let shared_state_ref = shared_state.clone();
    setup_outgoing_peer(addr, shared_state, true, on_receive_data_packet.clone()).await?;

    Ok(shared_state_ref)
}

// --- Called by the first peer in a P2P network. ---
// First of all, create a listener as a node in the P2P network. Second, connect to one of the nodes
// already integrated in the network and wait for the list with all peers. Connect to each individually
// in order to join.
// Everybody joining after us will be accepted via the listener, while everyone joining before we
// enter the network will be connected to manually.
pub async fn setup_ingoing_peer<F: Fn(Vec<u8>, SocketAddr) + Send + Sync + 'static>(port: u16,
    on_receive_data_packet: Arc<F>) -> Result<AM<PeerSharedState>, Box<dyn Error + Send + Sync>> {
    let listener_addr: SocketAddr = format!("0.0.0.0:{}", port).parse().unwrap();
    let listener = TcpListener::bind(listener_addr).await?;
    let shared_state = Arc::new(Mutex::new(PeerSharedState {
        listener_addr: listener_addr,
        peers: HashMap::new()
    }));
    // Separate reference for the afterworld
    let shared_state_ref = shared_state.clone();

    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((conn, addr)) => {
                    let shared_state_conn = shared_state.clone();
                    println!("Ingoing peer {} connected.", &addr);
                    handle_peer(conn, addr, shared_state_conn, on_receive_data_packet.clone())
                },
                Err(e) => {
                    eprintln!("Failed to accept new connection: {}.", e);
                    break;
                }
            }
        }
        println!("Terminating listener loop. Ingoing peer shut down.")
    });

    Ok(shared_state_ref)
}

// Called after setup_ingoing_peer(), in order to enter a P2P network
async fn setup_outgoing_peer<F: Fn(Vec<u8>, SocketAddr) + Send + Sync + 'static>(addr: SocketAddr, shared_state: AM<PeerSharedState>, request_peers: bool,
    on_receive_data_packet: Arc<F>) -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut conn = TcpStream::connect(addr).await?;

    // Send the handshake to pass port for ingoing connections and to fetch list of all peers
    conn.write_all(&construct_bantam_packet(HandshakePacket::new(
        shared_state.lock().await.listener_addr.port(), request_peers))).await?;
    handle_peer(conn, addr, shared_state.clone(), on_receive_data_packet);

    Ok(())
}

pub async fn shutdown(shared_state: AM<PeerSharedState>) -> Result<(), Box<dyn Error + Send + Sync>> {
    send_bantam_packet(ByePacket::new(0), shared_state).await
}

fn handle_peer<F: Fn(Vec<u8>, SocketAddr) + Send + Sync + 'static>(conn: TcpStream, addr: SocketAddr, shared_state: AM<PeerSharedState>,
    on_receive_data_packet: Arc<F>) {
    tokio::spawn(async move {
        if let Err(e) = handle_peer_io_loop(conn, addr, shared_state, on_receive_data_packet).await {
            eprintln!("Failed running IO loop for {}: {}", addr, e);
        }
    });
}

async fn handle_peer_io_loop<F: Fn(Vec<u8>, SocketAddr) + Send + Sync + 'static>(mut conn: TcpStream, addr: SocketAddr,
    shared_state: AM<PeerSharedState>, on_receive_data_packet: Arc<F>) -> Result<(), Box<dyn Error + Send + Sync>> {
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (mut reader, mut writer) = conn.split();
    
    loop {
        let mut buffer = [0u8; 2048];
        tokio::select! {
            Some(msg) = rx.recv() => {
                writer.write_all(&msg).await?;
            }
            read_result = reader.read(&mut buffer) => match read_result {
                Ok(bytes_read) if bytes_read > 0 => {
                    let packet_buffer = buffer[0..bytes_read].to_vec();
                    let (packet_type, mut stream) = deconstruct_bantam_packet(packet_buffer);
                    
                    match packet_type {
                        BantamPacketType::Handshake => {
                            // This will only occur on the ingoing (=listening) peer side
                            let handshake = HandshakePacket::from_stream(&mut stream);

                            // After hands have been shook, add the peer to the network
                            // We construct the listener address of the peer from the port they send us.
                            // This way, we can send new connections this peer's actual ingoing listener address.
                            let mut listener_addr = addr.clone();
                            listener_addr.set_port(handshake.listening_port);
                            shared_state.lock().await.add_peer(addr, Peer {
                                listener_addr, sender: tx.clone()
                            });

                            // If the ingoing peer requested a list of all P2P network members, fetch the list.
                            // If the peer didn't request such list, they already have a connection to an entry
                            // peer and used their peer address list to connect to us.
                            let peer_addresses = match handshake.request_peers {
                                true => {
                                    let mut peer_addresses = vec![];
                                    let shared_state_lock = shared_state.lock().await;
                                    for (_, peer) in shared_state_lock.peers.iter() {
                                        // If it's not the peer we're currently sending the peer list to...
                                        if peer.listener_addr != listener_addr {
                                            // ... then include the peer in the list.
                                            peer_addresses.push(SerializableSocketAddr::from_sock_addr(peer.listener_addr));
                                        }
                                    }

                                    println!("Peer {} connected. Sending address list...", &addr);
                                    peer_addresses
                                },
                                false => {
                                    println!("Peer {} connected.", &addr);
                                    vec![]
                                }
                            };

                            send_bantam_packet_to(HandshakeResponsePacket::new(peer_addresses),
                                addr, shared_state.clone()).await?;
                        },
                        BantamPacketType::HandshakeResponse => {
                            // This will only occur on the outgoing (=connecting) peer side

                            // As we now successfully connected, we can safely add the outgoing peer
                            shared_state.lock().await.add_peer(addr, Peer {
                                listener_addr: addr, sender: tx.clone()
                            });

                            let handshake_response = HandshakeResponsePacket::from_stream(&mut stream);
                            println!("Approved connection to outgoing peer successfully.");
                            // Connect to all peers that are in the P2P network, according to first-hand connection
                            if handshake_response.peers.len() > 0 {
                                println!("Connecting to remaining {} peers on the network...", handshake_response.peers.len());
                                for addr in handshake_response.peers {
                                    let sock_addr = addr.to_sock_addr();
                                    if !shared_state.lock().await.peers.contains_key(&sock_addr) {
                                        setup_outgoing_peer(sock_addr, shared_state.clone(), false, on_receive_data_packet.clone()).await?;
                                    }
                                }
                            }
                        },
                        BantamPacketType::Data => {
                            let data_packet = DataPacket::from_stream(&mut stream);
                            on_receive_data_packet(data_packet.bytes, addr);
                        },
                        BantamPacketType::Bye => {
                            println!("Peer {} manually disconnected.", &addr);
                            break
                        },
                    }
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => (), // Nothing to read
                Err(e) => {
                    // What to do?
                    eprintln!("Error occured while reading from {} stream: {}", &addr, e);
                    break
                },
                _ => ()
            }
        }
    }

    println!("Terminating IO loop for peer {}.", &addr);
    writer.shutdown().await?;
    shared_state.lock().await.remove_peer(addr);

    Ok(())
}

async fn send_packet(bytes: Vec<u8>, shared_state: AM<PeerSharedState>)
    -> Result<(), Box<dyn Error + Send + Sync>> {
    shared_state.lock().await.broadcast(bytes).await
}

async fn send_packet_to(bytes: Vec<u8>, addr: SocketAddr, shared_state: AM<PeerSharedState>)
    -> Result<(), Box<dyn Error + Send + Sync>> {
    shared_state.lock().await.unicast(bytes, addr).await
}

fn construct_bantam_packet<T: PacketHeader<BantamPacketType> + Serializable>(
    packet: T) -> Vec<u8> {
    let mut stream = BinaryStream::new();
    stream.write_packet_type(packet.get_type()).unwrap();
    packet.to_stream(&mut stream);
    
    stream.get_buffer_vec()
}

fn deconstruct_bantam_packet(bytes: Vec<u8>) -> (BantamPacketType, BinaryStream) {
    let mut stream = BinaryStream::from_bytes(&bytes);
    (stream.read_packet_type::<BantamPacketType>().unwrap(), stream)
}

async fn send_bantam_packet<T: PacketHeader<BantamPacketType> + Serializable>(
    packet: T, shared_state: AM<PeerSharedState>) -> Result<(), Box<dyn Error + Send + Sync>> {
    send_packet(construct_bantam_packet(packet), shared_state).await
}

async fn send_bantam_packet_to<T: PacketHeader<BantamPacketType> + Serializable>(packet: T,
    addr: SocketAddr, shared_state: AM<PeerSharedState>) -> Result<(), Box<dyn Error + Send + Sync>> {
    send_packet_to(construct_bantam_packet(packet), addr, shared_state).await
}

pub async fn send_data_packet(bytes: Vec<u8>, shared_state: AM<PeerSharedState>)
    -> Result<(), Box<dyn Error + Send + Sync>> {
    send_packet(construct_bantam_packet(DataPacket::new(bytes)), shared_state).await
}

pub async fn send_data_packet_to(bytes: Vec<u8>, addr: SocketAddr, shared_state: AM<PeerSharedState>)
    -> Result<(), Box<dyn Error + Send + Sync>>  {
    send_packet_to(construct_bantam_packet(DataPacket::new(bytes)), addr, shared_state).await
}
