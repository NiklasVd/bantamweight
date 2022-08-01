mod packet;
mod peer;
mod util;

use std::sync::Arc;
use tokio::sync::Mutex;

pub use packet::*;
pub use peer::*;
pub use util::*;

type AM<T> = Arc<Mutex<T>>;

#[cfg(test)]
mod tests {
    use std::{net::{Ipv6Addr, SocketAddr, SocketAddrV6}};
    use plain_binary_stream::{BinaryStream, Serializable};

    use crate::{HandshakeResponsePacket, SerializableSocketAddr};

    #[derive(Debug, Default, PartialEq)]
    struct Alpha(u32, Vec<u8>);
    
    impl Serializable for Alpha {
        fn to_stream(&self, stream: &mut BinaryStream) {
            stream.write_u32(self.0).unwrap();
            stream.write_byte_vec(&self.1).unwrap();
        }
    
        fn from_stream(stream: &mut BinaryStream) -> Self {
            Alpha(stream.read_u32().unwrap(), stream.read_byte_vec().unwrap())
        }
    }

    #[test]
    fn serialization_test() {
        let vec = vec![Alpha(1, vec![5, 10, 15]), Alpha(2, vec![100, 200, 255]), Alpha(3, vec![5, 7, 12])];
        let mut stream = BinaryStream::new();
        stream.write_vec(&vec).unwrap();

        let mut deserialization_stream = BinaryStream::from_buffer(stream.get_buffer());
        let vec_new_deserialized = deserialization_stream.read_vec::<Alpha>().unwrap();

        for i in 0..vec.len() {
            assert_eq!(vec[i], vec_new_deserialized[i])
        }
    }

    #[test]
    fn empty_vec_serialization_test() {
        let alpha = Alpha(0, vec![]);
        let mut stream = BinaryStream::new();
        alpha.to_stream(&mut stream);

        let mut deserialization_stream = BinaryStream::from_buffer(stream.get_buffer());
        let alpha_deserialized = Alpha::from_stream(&mut deserialization_stream);
        println!("A {:?} <=> A-Des. {:?}", alpha, alpha_deserialized);
        assert_eq!(alpha, alpha_deserialized)
    }

    #[test]
    fn socket_addr_serialization_test() {
        let sock_addr = SerializableSocketAddr::from_sock_addr(SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::new(0, 25, 0, 0, 0, 668, 0, 1), 8081, 0, 0)));
        let sock_addr2 = SerializableSocketAddr::from_sock_addr(SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::new(12312, 0, 0, 1, 0, 0, 0, 2), 8080, 0, 0)));

        let handshake_res_packet = HandshakeResponsePacket {
            peers: vec![sock_addr, sock_addr2]
        };
        let mut stream = BinaryStream::new();
        handshake_res_packet.to_stream(&mut stream);

        let mut deser_stream = BinaryStream::from_buffer(stream.get_buffer());
        let deser_handshake_res_packet = HandshakeResponsePacket::from_stream(&mut deser_stream);

        for i in 0..deser_handshake_res_packet.peers.len() {
            assert_eq!(handshake_res_packet.peers[i],
                deser_handshake_res_packet.peers[i]);
        }
    }
}
