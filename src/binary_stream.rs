use std::{collections::VecDeque, io::{self, Error}};
use crate::packet::Serializable;
use super::packet::PacketType;

// TODO: Implement checks for correct streaming mode
pub enum BinaryStreamMode {
    Read,
    Write
}

pub struct BinaryStream {
    pub mode: BinaryStreamMode,
    pub buffer: VecDeque<u8>
}

fn collect_array<T, I, const N: usize>(itr: I) -> [T; N]
where
    T: Default + Copy,
    I: IntoIterator<Item = T>,
{
    let mut res = [T::default(); N];
    for (it, elem) in res.iter_mut().zip(itr) {
        *it = elem
    }

    res
}

impl BinaryStream {
    pub fn new() -> BinaryStream {
        BinaryStream::with_capacity(None)
    }

    pub fn with_capacity(expected_size: Option<usize>) -> BinaryStream {
        let buffer = match expected_size {
            Some(size) => VecDeque::with_capacity(size),
            None => VecDeque::new()
        };

        BinaryStream {
            mode: BinaryStreamMode::Write,
            buffer
        }
    }

    pub fn from_buffer(buffer: VecDeque<u8>) -> BinaryStream {
        BinaryStream {
            mode: BinaryStreamMode::Read,
            buffer
        }
    }

    pub fn from_bytes(bytes: &[u8]) -> BinaryStream {
        BinaryStream {
            mode: BinaryStreamMode::Read,
            buffer: bytes.to_vec().into()
        }
    }

    pub fn size(&self) -> usize {
        self.buffer.len()
    }

    pub fn write_u32(&mut self, val: u32) -> io::Result<()> {
        self.write_buffer(&val.to_le_bytes().to_vec())
    }

    pub fn write_u64(&mut self, val: u64) -> io::Result<()> {
        self.write_buffer(&val.to_le_bytes().to_vec())
    }

    pub fn write_u128(&mut self, val: u128) -> io::Result<()> {
        self.write_buffer(&val.to_le_bytes().to_vec())
    }

    pub fn write_string(&mut self, val: &String) -> io::Result<()> {
        let string_bytes = val.as_bytes();
        self.write_u32(string_bytes.len() as u32).unwrap();
        self.write_buffer(&val.as_bytes().to_vec())
    }

    pub fn write_byte_vec(&mut self, val: &Vec<u8>) -> io::Result<()> {
        self.write_u32(val.len() as u32).unwrap();
        self.write_buffer(val)
    }

    pub fn write_vec<T: Serializable>(&mut self, val: &Vec<T>) -> io::Result<()> {
        self.write_u32(val.len() as u32)?;
        val.iter().for_each(|x| x.to_stream(self));
        Ok(())
    }

    pub fn write_packet_type(&mut self, val: PacketType) -> io::Result<()> {
        self.write_buffer_single(val.as_byte())
    }

    pub fn read_u32(&mut self) -> io::Result<u32> {
        let bytes = collect_array(self.read_buffer(4)?);
        Ok(u32::from_le_bytes(bytes))
    }

    pub fn read_u64(&mut self) -> io::Result<u64> {
        let bytes = collect_array(self.read_buffer(8)?);
        Ok(u64::from_le_bytes(bytes))
    }

    pub fn read_u128(&mut self) -> io::Result<u128> {
        let bytes = collect_array(self.read_buffer(16)?);
        Ok(u128::from_le_bytes(bytes))
    }

    pub fn read_string(&mut self) -> io::Result<String> {
        let string_size = self.read_u32()? as usize;
        Ok(String::from_utf8(self.read_buffer(string_size)?).unwrap())
    }

    pub fn read_byte_vec(&mut self) -> io::Result<Vec<u8>> {
        let vec_size = self.read_buffer_single()? as usize;
        self.read_buffer(vec_size)
    }

    pub fn read_vec<T: Serializable>(&mut self) -> io::Result<Vec<T>> {
        let vec_size = self.read_u32()?;
        let mut vec = Vec::with_capacity(vec_size as usize);
        for _ in 0..vec_size {
            vec.push(T::from_stream(self));
        }

        Ok(vec)
    }

    pub fn read_packet_type(&mut self) -> io::Result<PacketType> {
        Ok(PacketType::from_byte(self.read_buffer_single()?).unwrap())
    }

    pub fn write_buffer_single(&mut self, byte: u8) -> io::Result<()> {
        match self.mode {
            BinaryStreamMode::Read => Err(Error::new(io::ErrorKind::PermissionDenied, "Stream is in read-only mode")),
            BinaryStreamMode::Write => {
                self.buffer.push_back(byte);
                println!("Wrote 1 byte");
                Ok(())
            }
        }
    }

    pub fn write_buffer(&mut self, bytes: &Vec<u8>) -> io::Result<()> {
        match self.mode {
            BinaryStreamMode::Read => Err(Error::new(io::ErrorKind::PermissionDenied, "Stream is in read-only mode")),
            BinaryStreamMode::Write => {
                self.buffer.extend(bytes);
                println!("Wrote {} bytes", bytes.len());
                Ok(())
            }
        }
    }

    pub fn read_buffer_single(&mut self) -> io::Result<u8> {
        match self.mode {
            BinaryStreamMode::Write => Err(Error::new(io::ErrorKind::PermissionDenied, "Stream is in writing mode")),
            BinaryStreamMode::Read => {
                println!("Read 1 byte");
                Ok(self.buffer.pop_front().unwrap())
            }
        }
    }

    pub fn read_buffer(&mut self, count: usize) -> io::Result<Vec<u8>> {
        match self.mode {
            BinaryStreamMode::Write => Err(Error::new(io::ErrorKind::PermissionDenied, "Stream is in writing mode")),
            BinaryStreamMode::Read => {
                let mut bytes = vec![];
                for _ in 0..count {
                    bytes.push(self.buffer.pop_front().unwrap());
                }

                Ok(bytes)
            }
        }
    }
}
