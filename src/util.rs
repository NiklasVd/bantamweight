use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

use plain_binary_stream::{Serializable, BinaryStream};

#[derive(Debug, PartialEq)]
pub struct SerializableSocketAddr {
    pub ip: IpAddr,
    pub port: u16
}

impl Default for SerializableSocketAddr {
    fn default() -> Self {
        SerializableSocketAddr {
            ip: IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)),
            port: 0
        }
    }
}

fn byte_vec_to_short_vec(bytes: Vec<u8>) -> Vec<u16> {
    // As the segments of the IPv6 address are stored in u16, couple every pair of the 16 bytes,
    // so 8 in total together.
    // [x, x, x, x, x, x, x, x] to u16 ==> [0, 0, 0, 0, 0, 0, 0, 0, x, x, x, x, x, x, x, x]
    // Shift first of the two bytes 8 bits to the right
    // [0, 0, 0, 0, 0, 0, 0, 0, x, x, x, x, x, x, x, x] << 8 ==> [x, x, x, x, x, x, x, x, 0, 0, 0, 0, 0, 0, 0, 0]
    // Then merge the two via bitwise OR (|)
    let mut segments = vec![];
    for i in (0..bytes.len()).step_by(2) {
        let segment = ((bytes[i] as u16) << 8) | bytes[i + 1] as u16;
        segments.push(segment);
    }
    segments
}

impl Serializable for SerializableSocketAddr {
    fn to_stream(&self, stream: &mut BinaryStream) {
        let ip_bytes = match self.ip {
            IpAddr::V4(ip) => ip.octets().to_vec(),
            IpAddr::V6(ip) => ip.octets().to_vec()
        };
        stream.write_u16(self.port).unwrap();
        stream.write_byte_vec(&ip_bytes).unwrap();
    }

    fn from_stream(stream: &mut BinaryStream) -> Self {
        let port = stream.read_u16().unwrap();
        let ip_bytes = stream.read_byte_vec().unwrap();
        // If the byte vector has 16 bytes, its IPv6 (128 bit)
        let ip = match ip_bytes.len() {
            4 => Some(IpAddr::V4(Ipv4Addr::new(ip_bytes[0], ip_bytes[1], ip_bytes[2], ip_bytes[3]))),
            16 => {
                let segments = byte_vec_to_short_vec(ip_bytes);
                Some(IpAddr::V6(Ipv6Addr::new(segments[0], segments[1], segments[2],
                    segments[3], segments[4], segments[5], segments[6], segments[7])))
            },
            _ => None
        }.unwrap();

        SerializableSocketAddr::new(ip, port)
    }
}

impl SerializableSocketAddr {
    pub fn new(ip: IpAddr, port: u16) -> Self {
        SerializableSocketAddr {
            ip, port
        }
    }

    pub fn from_sock_addr(addr: SocketAddr) -> Self {
        Self::new(addr.ip(), addr.port())
    }

    pub fn to_sock_addr(&self) -> SocketAddr {
        match self.ip {
            IpAddr::V4(ip) => SocketAddr::V4(SocketAddrV4::new(ip, self.port)),
            IpAddr::V6(ip) => SocketAddr::V6(SocketAddrV6::new(ip, self.port, 0, 0))
        }
    }
}