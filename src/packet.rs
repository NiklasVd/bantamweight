use crate::{BinaryStream, SerializableSocketAddr};

pub trait PacketType : Sized {
    fn to_byte(&self) -> u8;
    fn from_byte(byte: u8) -> Option<Self>;
}

pub trait Serializable {
    fn to_stream(&self, stream: &mut BinaryStream);
    fn from_stream(stream: &mut BinaryStream) -> Self;
}

pub trait PacketHeader<T: PacketType> {
    fn get_type(&self) -> T;
}

pub trait Packet<T: PacketType> : Serializable + PacketHeader<T> {
}

#[derive(Debug, PartialEq)]
pub enum BantamPacketType {
    Handshake = 0,
    HandshakeResponse = 1,
    Data = 2,
    Bye = 3
}

impl PacketType for BantamPacketType {
    fn to_byte(&self) -> u8 {
        match self {
            BantamPacketType::Handshake => 0,
            BantamPacketType::HandshakeResponse => 1,
            BantamPacketType::Data => 2,
            BantamPacketType::Bye => 3
        }
    }

    fn from_byte(byte: u8) -> Option<BantamPacketType> {
        Some(match byte {
            0 => BantamPacketType::Handshake,
            1 => BantamPacketType::HandshakeResponse,
            2 => BantamPacketType::Data,
            3 => BantamPacketType::Bye,
            _ => return None
        })
    }
}

pub struct HandshakePacket {
    listening_port: u16
}

pub struct HandshakeResponsePacket {
    pub peers: Vec<SerializableSocketAddr>
}

struct ByePacket {
}

impl Serializable for HandshakePacket {
    fn to_stream(&self, stream: &mut BinaryStream) {
        stream.write_u16(self.listening_port).unwrap();
    }

    fn from_stream(stream: &mut BinaryStream) -> Self {
        HandshakePacket::new(stream.read_u16().unwrap())
    }
}

impl PacketHeader<BantamPacketType> for HandshakePacket {
    fn get_type(&self) -> BantamPacketType {
        BantamPacketType::Handshake
    }
}

impl HandshakePacket {
    pub fn new(listening_port: u16) -> HandshakePacket {
        HandshakePacket {
            listening_port
        }
    }
}

impl Serializable for HandshakeResponsePacket {
    fn to_stream(&self, stream: &mut BinaryStream) {
        stream.write_vec(&self.peers).unwrap();
    }

    fn from_stream(stream: &mut BinaryStream) -> Self {
        HandshakeResponsePacket {
            peers: stream.read_vec::<SerializableSocketAddr>().unwrap()
        }
    }
}

impl PacketHeader<BantamPacketType> for HandshakeResponsePacket {
    fn get_type(&self) -> BantamPacketType {
        BantamPacketType::HandshakeResponse
    }
}

impl HandshakeResponsePacket {
    pub fn new(peers: Vec<SerializableSocketAddr>) -> HandshakeResponsePacket {
        HandshakeResponsePacket {
            peers
        }
    }
}
