use std::{collections::HashMap, error::Error, io::ErrorKind, net::SocketAddr, sync::Arc};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::{TcpListener, TcpStream}, sync::{mpsc, Mutex}};
use crate::{AM, BantamPacketType, BinaryStream, ByePacket, DataPacket, HandshakePacket, HandshakeResponsePacket, PacketHeader, PacketType, Serializable, SerializableSocketAddr};

type Tx = mpsc::UnboundedSender<Vec<u8>>;

pub struct Peer {
    listening_port: u16,
    sender: Tx
}

pub struct PeerSharedState {
    listening_port: u16,
    pub peers: HashMap<SocketAddr, Peer> // The pub is temporary!
}

impl PeerSharedState {
    async fn broadcast(&self, bytes: Vec<u8>) -> Result<(), Box<dyn Error + Send + Sync>> {
        Ok(for peer in self.peers.iter() {
            peer.1.sender.send(bytes.clone())? // Get rid of clone
        })
    }

    async fn unicast(&self, bytes: Vec<u8>, addr: SocketAddr) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.peers.get(&addr).unwrap().sender.send(bytes)?;
        Ok(())
    }
}

pub async fn setup_peer(port: u16, addr: SocketAddr) -> Result<AM<PeerSharedState>, Box<dyn Error + Send + Sync>> {
    let shared_state = setup_ingoing_peer(port).await?;
    let shared_state_ref = shared_state.clone();
    setup_outgoing_peer(addr, shared_state).await?;

    Ok(shared_state_ref)
}

// --- Called as a first of all peer in a P2P network. ---
// First of all, create a listener as a node in the P2P network. Second, connect to one of the nodes
// already integrated in the network and wait for the list with all peers. Connect to each individually
// in order to join.
// Everybody joining after us will be accepted via the listener, while everyone joining before we
// enter the network will be connected to manually.
pub async fn setup_ingoing_peer(port: u16) -> Result<AM<PeerSharedState>, Box<dyn Error + Send + Sync>> {
    let listener = TcpListener::bind(format!("0.0.0.0:{}", port)).await?;
    let shared_state = Arc::new(Mutex::new(PeerSharedState {
        listening_port: port,
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
                    handle_peer(conn, addr, shared_state_conn)
                },
                Err(e) => {
                    eprintln!("Failed to accept new connection: {}.", e);
                    break;
                }
            }
        }
    });

    Ok(shared_state_ref)
}

fn handle_peer(conn: TcpStream, addr: SocketAddr, shared_state: AM<PeerSharedState>) {
    tokio::spawn(async move {
        if let Err(e) = handle_peer_io_loop(conn, addr, shared_state).await {
            eprintln!("Failed running IO loop for {}: {}", addr, e);
        }
    });
}

async fn handle_peer_io_loop(mut conn: TcpStream, addr: SocketAddr, shared_state: AM<PeerSharedState>) -> Result<(), Box<dyn Error + Send + Sync>> {
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (mut reader, mut writer) = conn.split();
    loop {
        // Use tokio::select! macro?
        // Write any queued messages from the channel into the tcp-stream.
        if let Some(msg) = rx.recv().await {
            println!("Writing message from queue. Type: {:?}",
                BantamPacketType::from_byte(*msg.get(msg.len() - 1).unwrap()));
            writer.write_all(&msg).await?;
        }

        println!("Reading stream with {}.", &addr);

        // Read any messages from the tcp-stream.
        let mut buffer = [0u8; 2048];
        match reader.read(&mut buffer).await {
            Ok(bytes_read) => {
                let packet_buffer = buffer[0..bytes_read].to_vec();
                let (packet_type, mut stream) = deconstruct_bantam_packet(packet_buffer);
                println!("Received a packet of type {:?}, size {}b from {}.", &packet_type, &bytes_read, &addr);

                match packet_type {
                    BantamPacketType::Handshake => {
                        // This will only occur on the ingoing peer side
                        let handshake = HandshakePacket::from_stream(&mut stream);
                        println!("Received handshake from {}. Port = {}, Request peers = {}.", &addr, &handshake.listening_port, &handshake.request_peers);
                        
                        if handshake.request_peers {
                            // Convert all socket addresses to their serializable counterpart
                            let peer_ser_addresses = shared_state.lock().await.peers.iter().map(|(addr, peer)|
                            SerializableSocketAddr::new(addr.ip(), peer.listening_port)).collect::<Vec<SerializableSocketAddr>>();
                            println!("Sent handshake response with {} addresses to ingoing connection {}.", peer_ser_addresses.len(), addr);
                            send_bantam_packet_to(HandshakeResponsePacket::new(peer_ser_addresses), addr, shared_state.clone()).await?;
                        }
                    
                        shared_state.lock().await.peers.insert(addr, Peer {
                            listening_port: handshake.listening_port, sender: tx.clone()
                        });
                        println!("Added {} to peers.", &addr);
                    },
                    BantamPacketType::HandshakeResponse => {
                        // This will only occur on the outgoing peer side

                        println!("Received handshake response. Connecting to peers...");
                        // As we now successfully connected, we can safely add the outgoing peer
                        shared_state.lock().await.peers.insert(addr, Peer {
                            listening_port: addr.port(), sender: tx.clone()
                        });

                        let handshake_response = HandshakeResponsePacket::from_stream(&mut stream);
                        // Connect to all peers that are in the P2P network, according to first-hand connection
                        for addr in handshake_response.peers {
                            println!("Establishing connection with forwarded peer {:?}.", &addr);
                            //setup_peer(addr.to_sock_addr(), shared_state.clone()).await;
                            connect_to_peer(addr.to_sock_addr(), shared_state.clone()).await?;
                        }
                    },
                    BantamPacketType::Data => {
                        // Critical point. How to communicate to outer world that a data packet has been received?
                    },
                    BantamPacketType::Bye => {
                        println!("Peer {} manually disconnected.", &addr);
                        break
                    },
                }
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => (), // Nothing to read
            Err(e) => {
                eprintln!("Error occured while reading from {} stream: {}", &addr, e)
            }
        };
    }

    println!("Terminating IO loop for peer {}.", &addr);
    Ok(())
}

// Called after setup_listener(), in order to enter a P2P network
async fn setup_outgoing_peer(addr: SocketAddr, shared_state: AM<PeerSharedState>) -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut conn = TcpStream::connect(addr).await?;
    let  shared_state_lock = shared_state.lock().await;

    // Send the handshake
    println!("Connected to {}. Sending handshake...", addr);
    // As we aren't running the IO loop yet, we'll have to write to the stream manually
    conn.write_all(&construct_bantam_packet(HandshakePacket::new(shared_state_lock.listening_port, true))).await?;

    Ok(())
}

async fn connect_to_peer(addr: SocketAddr, shared_state: AM<PeerSharedState>) -> Result<(), Box<dyn Error + Send + Sync>> {
    let conn = TcpStream::connect(addr).await?;
    handle_peer(conn, addr, shared_state);

    Ok(())
}

pub async fn shutdown(shared_state: AM<PeerSharedState>) -> Result<(), Box<dyn Error + Send + Sync>> {
    send_bantam_packet(ByePacket::new(0), shared_state).await
}

async fn send_packet(bytes: Vec<u8>, shared_state: AM<PeerSharedState>) -> Result<(), Box<dyn Error + Send + Sync>> {
    shared_state.lock().await.broadcast(bytes).await
}

async fn send_packet_to(bytes: Vec<u8>, addr: SocketAddr, shared_state: AM<PeerSharedState>) -> Result<(), Box<dyn Error + Send + Sync>> {
    shared_state.lock().await.unicast(bytes, addr).await
}

fn construct_bantam_packet<T: PacketHeader<BantamPacketType> + Serializable>(
    packet: T) -> Vec<u8> {
    let mut stream = BinaryStream::new();
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

async fn send_bantam_packet_to<T: PacketHeader<BantamPacketType> + Serializable>(
    packet: T, addr: SocketAddr, shared_state: AM<PeerSharedState>) -> Result<(), Box<dyn Error + Send + Sync>> {
    send_packet_to(construct_bantam_packet(packet), addr, shared_state).await
}

pub async fn send_data_packet(bytes: Vec<u8>, shared_state: AM<PeerSharedState>) -> Result<(), Box<dyn Error + Send + Sync>> {
    send_packet(construct_bantam_packet(DataPacket::new(bytes)), shared_state).await
}

pub async fn send_data_packet_to(bytes: Vec<u8>, addr: SocketAddr, shared_state: AM<PeerSharedState>)
    -> Result<(), Box<dyn Error + Send + Sync>>  {
    send_packet_to(construct_bantam_packet(DataPacket::new(bytes)), addr, shared_state).await
}
