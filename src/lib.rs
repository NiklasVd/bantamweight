mod binary_stream;
mod packet;

pub use binary_stream::*;
pub use packet::*;

#[cfg(test)]
mod tests {
    #[test]
    fn basic_serialization_test() {
    }
}
