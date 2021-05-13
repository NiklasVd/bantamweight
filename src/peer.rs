use std::{collections::HashMap, error::Error, io::ErrorKind, mem, net::SocketAddr, sync::Arc};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::{TcpListener, TcpStream}, sync::{mpsc, Mutex}};
use crate::{AM, BantamPacketType, BinaryStream, HandshakePacket, HandshakeResponsePacket, PacketHeader, Serializable, SerializableSocketAddr};

type Tx = mpsc::UnboundedSender<Vec<u8>>;
type Rx = mpsc::UnboundedReceiver<Vec<u8>>;

struct Peer {
    sender: Tx
}

struct PeerSharedState {
    port: u16,
    peers: HashMap<SocketAddr, Peer>
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

// First of all, create a listener as a node in the P2P network. Second, connect to one of the nodes
// already integrated in the network and wait for the list with all peers. Connect to each individually
// in order to join.
// Everybody joining after us will be accepted via the listener, while everyone joining before we
// enter the network will be connected to manually.
async fn setup_ingoing_peer(port: u16) -> Result<(), Box<dyn Error>> {
    let listener = TcpListener::bind(format!("0.0.0.0:{}", port)).await?;
    let shared_state = Arc::new(Mutex::new(PeerSharedState {
        port,
        peers: HashMap::new()
    }));

    loop {
        let (conn, addr) = listener.accept().await?;
        let shared_state_conn = shared_state.clone();
        println!("{} connected.", &addr);
        
        tokio::spawn(async move {
            if let Err(e) = handle_ingoing_peer(conn, addr, shared_state_conn).await {
                eprint!("Error occured when handling accepted connection: {}", e);
            }
        });
    }
}

async fn handle_ingoing_peer(conn: TcpStream, addr: SocketAddr,
    shared_state: Arc<Mutex<PeerSharedState>>) -> Result<(), Box<dyn Error + Send + Sync>> {
    let (tx, rx) = mpsc::unbounded_channel::<Vec<u8>>();
    
    // Receive handshake packet
    let foo_received_handshake = true;

    let mut shared_state_guard = shared_state.lock().await;
    // As soon as the handshake is received, send a response back
    if foo_received_handshake {
        let peers = shared_state_guard.peers.keys();
        // Convert all socket addresses to their serializable counterpart
        // Next step: Wait for handshake packet which tells us the port the respective peer is listening on.
        // That is important when sending his contact address out to new peers connecting afterwards, so they
        // can find this peer.
        let peer_ser_addresses = peers.map(|&addr|
                SerializableSocketAddr::from_sock_addr(addr)).collect::<Vec<SerializableSocketAddr>>();
        println!("Sent handshake response with {} addresses to ingoing connection {}.", peer_ser_addresses.len(), addr);
        send_bantam_message_to(HandshakeResponsePacket::new(peer_ser_addresses), addr, shared_state.clone()).await?;
    }

    // Update the list of network members
    shared_state_guard.peers.insert(addr, Peer {
        sender: tx
    });
    println!("Added {} to peers.", &addr);
    mem::drop(shared_state_guard);

    handle_peer_io_loop(conn, addr, rx);
    Ok(())
}

async fn handle_peer_io_loop(mut conn: TcpStream, addr: SocketAddr, mut rx: Rx) -> Result<(), Box<dyn Error + Send + Sync>> {
    let (mut reader, mut writer) = conn.split();
    loop {
        // Use tokio::select! macro?
        // Write any queued messages from the channel into the tcp-stream.
        if let Some(msg) = rx.recv().await {
            println!("Writing message from queue");
            writer.write_all(&msg).await?;
        }

        // Read any messages from the tcp-stream.
        let mut buffer = [0u8; 2048];
        match reader.read(&mut buffer).await {
            Ok(bytes_read) => {
                let packet_buffer = buffer[0..bytes_read].to_vec();
                let (packet_type, stream) = deconstruct_bantam_message(packet_buffer);
                println!("Received a packet of type {:?} from {}.", &packet_type, &addr);

                match packet_type {
                    BantamPacketType::Data => {
                        // Critical point. How to communicate to outer world that a data packet has been received?
                    },
                    BantamPacketType::Bye => {
                        println!("Peer {} manually disconnected.", &addr);
                        break
                    }
                    _ => () // Handshake must be send at very beginning and ingoing peer wouldn't send a handshake response
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

async fn send_message(bytes: Vec<u8>, shared_state: AM<PeerSharedState>) -> Result<(), Box<dyn Error + Send + Sync>> {
    shared_state.lock().await.broadcast(bytes).await
}

async fn send_message_to(bytes: Vec<u8>, addr: SocketAddr, shared_state: AM<PeerSharedState>) -> Result<(), Box<dyn Error + Send + Sync>> {
    shared_state.lock().await.unicast(bytes, addr).await
}

fn construct_bantam_message<T: PacketHeader<BantamPacketType> + Serializable>(
    packet: T) -> Vec<u8> {
    let mut stream = BinaryStream::new();
    packet.to_stream(&mut stream);
    stream.get_buffer_vec()
}

fn deconstruct_bantam_message(bytes: Vec<u8>) -> (BantamPacketType, BinaryStream) {
    let mut stream = BinaryStream::from_bytes(&bytes);
    (stream.read_packet_type::<BantamPacketType>().unwrap(), stream)
}

async fn send_bantam_message<T: PacketHeader<BantamPacketType> + Serializable>(
    packet: T, shared_state: AM<PeerSharedState>) -> Result<(), Box<dyn Error + Send + Sync>> {
    send_message(construct_bantam_message(packet), shared_state).await
}

async fn send_bantam_message_to<T: PacketHeader<BantamPacketType> + Serializable>(
    packet: T, addr: SocketAddr, shared_state: AM<PeerSharedState>) -> Result<(), Box<dyn Error + Send + Sync>> {
    send_message_to(construct_bantam_message(packet), addr, shared_state).await
}


// Called after setup_listener(), in order to enter a P2P network
async fn setup_outgoing_peer(addr: SocketAddr, shared_state: AM<PeerSharedState>) -> Result<(), Box<dyn Error + Send + Sync>> {
    // tokio::spawn(async move {
    //     if let Err(e) = connect_to_peer(addr, shared_state).await {
    //         eprintln!("Error occured while connecting to peer {}: {}.", addr, e);
    //     }
    // });

    let mut conn = TcpStream::connect(addr).await?;
    let  shared_state_lock = shared_state.lock().await;
    println!("Connected to {}. Sending handshake...", addr);
    send_bantam_message(HandshakePacket::new(shared_state_lock.port), shared_state.clone()).await?;

    
    // Read and block until the handshake response is received
    let mut buffer = [0u8; 1024];
    match conn.read(&mut buffer).await {
        Ok(bytes_read) => {
            let bytes = buffer[0..bytes_read].to_vec();
            let (packet_type, mut stream) = deconstruct_bantam_message(bytes);
            if packet_type == BantamPacketType::HandshakeResponse {
                let handshake_response = HandshakeResponsePacket::from_stream(&mut stream);
                // Connect to all peers that are in the P2P network, according to first-hand connection
                for addr in handshake_response.peers {
                    //setup_peer(addr.to_sock_addr(), shared_state.clone()).await;
                    connect_to_peer(addr.to_sock_addr(), shared_state.clone()).await;
                }
            }
            else {
                panic!("Failed to receive handshake response from outgoing peer {}.", &addr)
            }
        }
        Err(e) => {
            eprintln!("Failed to receive handshake response.");
            return Err(Box::new(e));
        }
    }


    Ok(())
}

async fn connect_to_peer(addr: SocketAddr, shared_state: AM<PeerSharedState>) -> Result<(), Box<dyn Error + Send + Sync>> {
    let conn = TcpStream::connect(addr).await?;
    handle_outgoing_peer(conn, addr, shared_state).await
}

async fn handle_outgoing_peer(conn: TcpStream, addr: SocketAddr, shared_state: AM<PeerSharedState>)
    -> Result<(), Box<dyn Error + Send + Sync>> {
        tokio::spawn(async move {
        let (tx, rx) = mpsc::unbounded_channel::<Vec<u8>>();
        // Add this peer to the list
        shared_state.lock().await.peers.insert(addr, Peer {
            sender: tx
        });
        
        handle_peer_io_loop(conn, addr, rx);
    });
    Ok(())
}

async fn shutdown(shared_state: AM<PeerSharedState>) -> Result<(), Box<dyn Error + Send + Sync>> {
    send_bantam_message(Bye, shared_state)
}
