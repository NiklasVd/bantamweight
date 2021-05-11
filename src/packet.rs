use crate::BinaryStream;

#[derive(Debug)]
pub enum PacketType {
    Blockchain = 0,
    RequestBlockchain = 1,
    Block = 2,
    Transaction = 3
}

impl PacketType {
    pub fn as_byte(&self) -> u8 {
        match self {
            PacketType::Blockchain => 0,
            PacketType::RequestBlockchain => 1,
            PacketType::Block => 2,
            PacketType::Transaction => 3
        }
    }

    pub fn from_byte(byte: u8) -> Option<PacketType> {
        Some(match byte {
            0 => PacketType::Blockchain,
            1 => PacketType::RequestBlockchain,
            2 => PacketType::Block,
            3 => PacketType::Transaction,
            _ => return None
        })
    }
}

pub trait Serializable {
    fn to_stream(&self, stream: &mut BinaryStream);
    fn from_stream(stream: &mut BinaryStream) -> Self;
}

pub trait Packet {
    fn get_type(&self) -> PacketType;
}

pub trait ClientPacket : Serializable + Packet {
}

