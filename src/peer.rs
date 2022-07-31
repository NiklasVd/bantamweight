use std::{collections::HashMap, error::Error, fmt, io::ErrorKind, net::SocketAddr, sync::Arc, time::Duration};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::{TcpListener, TcpStream, tcp::ReadHalf}, sync::{mpsc, Mutex}, time::timeout};
use crate::{AM, BantamPacketType, BinaryStream, ByePacket, DataPacket, HandshakePacket, HandshakeResponsePacket, PacketHeader, Serializable, SerializableSocketAddr};

pub const TCP_STREAM_READ_BUFFER_SIZE: usize = 256;
pub const TCP_STREAM_CONNECTION_TIMEOUT_SECS: u64 = 15;
pub const TCP_STREAM_READ_TIMEOUT_SECS: u64 = 5;
type Tx = mpsc::UnboundedSender<Vec<u8>>;

#[derive(Debug)]
pub enum BantamError {
    ConnectionTimeout,
    ReadTimeout,
    ReceivedCorruptedPacket
}

impl fmt::Display for BantamError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.to_string())
    }
}

impl Error for BantamError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        None
    }

    fn description(&self) -> &str {
        "description() is deprecated; use Display"
    }

    fn cause(&self) -> Option<&dyn Error> {
        None
    }
}

pub struct Peer {
    // Add ingoing/outgoing peer tag?
    pub listener_addr: SocketAddr,
    sender: Tx
}

pub trait ExternalSharedState {
    fn on_connected(&mut self, addr: SocketAddr);
    fn on_disconnected(&mut self, addr: SocketAddr); // E.g., so that miner can remove blockchain requests from the list
    fn on_receive_packet(&mut self, bytes: Vec<u8>, sender_addr: SocketAddr);
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

    pub fn get_peer_count(&self) -> usize {
        return self.peers.len()
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
        else {
            eprintln!("Peer with address {} is not part of this network.", addr);
        }
        
        Ok(())
    }
}

// --- Called by a peer that wants to connect ot a P2P network. ---
pub async fn setup_peer<T: ExternalSharedState + Send + Sync + 'static>(port: u16, addr: SocketAddr,
    ext_shared_state: AM<T>) -> Result<AM<PeerSharedState>, Box<dyn Error + Send + Sync>> {
    let shared_state = setup_ingoing_peer(port, ext_shared_state.clone()).await?;
    let shared_state_ref = shared_state.clone();
    setup_outgoing_peer(addr, shared_state, true, ext_shared_state.clone()).await?;

    Ok(shared_state_ref)
}

// --- Called by the first peer in a P2P network. ---
// First of all, create a listener as a node in the P2P network. Second, connect to one of the nodes
// already integrated in the network and wait for the list with all peers. Connect to each individually
// in order to join.
// Everybody joining after us will be accepted via the listener, while everyone joining before we
// enter the network will be connected to manually.
pub async fn setup_ingoing_peer<T: ExternalSharedState + Send + Sync + 'static>(port: u16,
    ext_shared_state: AM<T>) -> Result<AM<PeerSharedState>, Box<dyn Error + Send + Sync>> {
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
                    println!("Accepted connection with {}...", &addr);

                    handle_peer(conn, addr, shared_state_conn, ext_shared_state.clone())
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
async fn setup_outgoing_peer<T: ExternalSharedState + Send + Sync + 'static>(addr: SocketAddr,
    shared_state: AM<PeerSharedState>, request_peers: bool, ext_shared_state: AM<T>) -> Result<(), Box<dyn Error + Send + Sync>> {
    // Connect to TCP stream until timeout interrupts the operation.
    match timeout(Duration::from_secs(TCP_STREAM_CONNECTION_TIMEOUT_SECS),
        TcpStream::connect(addr)).await {
        Ok(connection_result) => {
            match connection_result {
                Ok(mut conn) => {
                    // Send the handshake to pass port for ingoing connections and to fetch list of all peers
                    conn.write_all(&construct_bantam_packet(HandshakePacket::new(
                    shared_state.lock().await.listener_addr.port(), request_peers))).await?;
                    println!("Connecting with {}...", &addr);
                    handle_peer(conn, addr, shared_state.clone(), ext_shared_state);

                    Ok(())
                },
                Err(e) => return Err(Box::new(e))
            }
        },
        Err(_) => {
            return Err(Box::new(BantamError::ConnectionTimeout));
        }
    }
}

pub async fn shutdown(shared_state: AM<PeerSharedState>) -> Result<(), Box<dyn Error + Send + Sync>> {
    send_bantam_packet(ByePacket::new(0), shared_state).await
}

fn handle_peer<T: ExternalSharedState + Send + Sync + 'static>(conn: TcpStream, addr: SocketAddr,
    shared_state: AM<PeerSharedState>, ext_shared_state: AM<T>) {
    // Configure the connection
    if let Err(e) = conn.set_linger(None) {
        println!("Failed to set linger duration of connection with {}: {}.", addr, e);
    }
    if let Err(e) = conn.set_nodelay(true) {
        println!("Failed to set no delay of connection with {}: {}.", addr, e);
    }

    tokio::spawn(async move {
        if let Err(e) = handle_peer_io_loop(conn, addr, shared_state, ext_shared_state).await {
            eprintln!("Failed running IO loop for {}: {}", addr, e);
        }
    });
}

async fn handle_peer_io_loop<T: ExternalSharedState + Send + Sync + 'static>(mut conn: TcpStream, addr: SocketAddr,
    shared_state: AM<PeerSharedState>, ext_shared_state: AM<T>) -> Result<(), Box<dyn Error + Send + Sync>> {
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (mut reader, mut writer) = conn.split();

    loop {
        let mut buffer = [0u8; TCP_STREAM_READ_BUFFER_SIZE];
        tokio::select! {
            Some(msg) = rx.recv() => {
                writer.write_all(&msg).await?;
            }
            read_result = reader.read(&mut buffer) => match read_result {
                Ok(bytes_received) => {
                    let mut buffer_vec = buffer[0..bytes_received].to_vec();
                    // Check the segment read first and potentially read remaining segments of the whole packet.
                    match read_segments(bytes_received, &mut buffer_vec, &mut reader).await {
                        Err(e) => {
                            eprintln!("Error occured while reading packet segments of {}: {}", addr, e);
                            break;
                        },
                        Ok(total_bytes_received) if total_bytes_received == 0 => break,
                        _ => ()
                    }

                    // Process packet depending on BantamPacketType
                    match process_packet(buffer_vec, addr, tx.clone(), shared_state.clone(), ext_shared_state.clone()).await {
                        Ok(connected) if !connected => break, // If the function returns Ok(false), peer sent a ByePacket
                        Err(e) => {
                            eprintln!("Error occured while processing received packet: {}.", e);
                            break
                        },
                        _ => ()
                    }
                },
                Err(e) if e.kind() == ErrorKind::ConnectionReset => break, // Peer disconnected
                Err(e) => {
                    eprintln!("Error ({:?}) occured while reading stream of {}: {}", e.kind(), &addr, e);
                    break
                }
            }
        }
    }

    println!("Peer {} disconnected.", &addr);
    ext_shared_state.lock().await.on_disconnected(addr);
    shared_state.lock().await.remove_peer(addr);

    Ok(())
}

async fn read_segments<'a>(bytes_received: usize, buffer_vec: &mut Vec<u8>, reader: &mut ReadHalf<'a>)
    -> Result<usize, Box<dyn Error + Send + Sync>> {
    if bytes_received <= 4 { // As the base bantam packet header (size of packet) is 4 bytes big, a valid message must have > 4 bytes.
        return Ok(0);
    }

    let total_packet_size = check_first_packet_segment(buffer_vec);
    let mut total_packet_bytes_received = bytes_received - 4;
    let packet_bytes_received_percent_step = (total_packet_size as f32 * 0.25f32) as usize;
    let mut packet_bytes_received_step = packet_bytes_received_percent_step;

    // As long as the amount of bytes we read is still less than the size announced by the packet, continue reading...
    while total_packet_bytes_received < total_packet_size {
        let mut buffer = [0u8; TCP_STREAM_READ_BUFFER_SIZE];
        match timeout(Duration::from_secs(TCP_STREAM_READ_TIMEOUT_SECS), reader.read(&mut buffer)).await {
            Ok(read_result) => {
                match read_result {
                    Ok(bytes_received) => {
                        buffer_vec.extend(buffer[0..bytes_received].to_vec());
                        total_packet_bytes_received += bytes_received;
                        
                        let progress_percentage = f32::round(total_packet_bytes_received as f32 /
                            total_packet_size as f32 * 100f32);
                        if total_packet_bytes_received > packet_bytes_received_step {
                            println!("Downloaded {}% of packet ({}b of {}b).", progress_percentage,
                                total_packet_bytes_received, total_packet_size);
                            packet_bytes_received_step += packet_bytes_received_percent_step;
                        }
                    },
                    Err(e) => return Err(Box::new(e))
                }   
            },
            Err(_) => return Err(Box::new(BantamError::ReadTimeout))
        }
    }

    Ok(total_packet_size)
}

fn check_first_packet_segment(buffer: &mut Vec<u8>) -> usize {
    let mut size_bytes = [0u8; 4];
    // Remove the first four elements of the buffer, i.e., the u32 at the beginning of the first packet segment,
    // which indicates the total packet size.
    for i in 0..4 {
        size_bytes[i] = buffer.remove(0);
    }

    u32::from_le_bytes(size_bytes) as usize
}

async fn process_packet<T: ExternalSharedState + Send + Sync + 'static>(bytes: Vec<u8>, addr: SocketAddr,
    tx: Tx, shared_state: AM<PeerSharedState>, ext_shared_state: AM<T>) -> Result<bool, Box<dyn Error + Send + Sync>> {
    let (packet_type, mut stream) = deconstruct_bantam_packet(bytes);
    match packet_type {
        BantamPacketType::Handshake => {
            process_handshake_packet(stream, addr, tx.clone(), shared_state.clone()).await?;
        },
        BantamPacketType::HandshakeResponse => {
            process_handshake_response_packet(stream, addr, tx.clone(), shared_state.clone(),
                ext_shared_state.clone()).await?;
        },
        BantamPacketType::Data => {
            let data_packet = DataPacket::from_stream(&mut stream);
            ext_shared_state.lock().await.on_receive_packet(data_packet.bytes, addr);
        },
        BantamPacketType::Bye => {
            println!("Peer {} manually disconnected.", &addr);
            return Ok(false);
        },
    }

    Ok(true)
}

async fn process_handshake_packet(mut stream: BinaryStream, addr: SocketAddr, tx: Tx,
    shared_state: AM<PeerSharedState>) -> Result<(), Box<dyn Error + Send + Sync>> {
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
            for (_, peer) in shared_state.lock().await.peers.iter() {
                // If it's not the peer we're currently sending the peer list to...
                if peer.listener_addr != listener_addr {
                    // ... then include the peer in the list.
                    peer_addresses.push(SerializableSocketAddr::from_sock_addr(peer.listener_addr));
                }
            }

            println!("Peer {} connected. Integrating into network...", &addr);
            peer_addresses
        },
        false => {
            println!("Peer {} connected.", &addr);
            vec![]
        }
    };

    // Ingoing peer is obligated to send handshake response.
    send_bantam_packet_to(HandshakeResponsePacket::new(peer_addresses),
        addr, shared_state.clone()).await
}

async fn process_handshake_response_packet<T: ExternalSharedState + Send + Sync + 'static>(
    mut stream: BinaryStream, addr: SocketAddr, tx: Tx, shared_state: AM<PeerSharedState>,
    ext_shared_state: AM<T>) -> Result<(), Box<dyn Error + Send + Sync>> {
    // This will only occur on the outgoing (=connecting) peer side

    // As we now successfully connected, we can safely add the outgoing peer
    shared_state.lock().await.add_peer(addr, Peer {
        listener_addr: addr, sender: tx.clone()
    });

    // Outgoing peer is obligated to send handshake
    let handshake_response = HandshakeResponsePacket::from_stream(&mut stream);
    // Connect to all peers that are in the P2P network, according to first-hand connection
    if handshake_response.peers.len() > 0 {
        println!("Connecting with remaining peers in the network...");
        for addr in handshake_response.peers {
            let sock_addr = addr.to_sock_addr();
            if !shared_state.lock().await.peers.contains_key(&sock_addr) {
                if let Err(e) = setup_outgoing_peer(sock_addr, shared_state.clone(), false, ext_shared_state.clone()).await {
                    println!("Connection attempt with {} failed: {}.", sock_addr, e);
                    continue;
                }
            }
        }
    }

    println!("Established connection with {}.", &addr);
    ext_shared_state.lock().await.on_connected(addr);
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
    
    let buffer = stream.get_buffer_vec();
    // Write the total packet size for the receiver, so they know how much to read.
    let mut header = u32::to_le_bytes(buffer.len() as u32).to_vec();
    // We need the packet size indicator to be at the very beginning of the packet, so it can be read first.
    header.extend(buffer);
    header
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
