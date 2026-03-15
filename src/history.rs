//! Bounded circular buffer for session history.
//!
//! Prevents unbounded memory growth by wrapping around when capacity is reached.
//! Default capacity is 10MB which is sufficient for most terminal sessions.

// Re-export from constants
pub use crate::constants::DEFAULT_HISTORY_CAPACITY;

/// A circular buffer that overwrites old data when capacity is reached.
///
/// This is used for storing terminal output history with bounded memory usage.
/// When the buffer is full, new data overwrites the oldest data.
#[derive(Debug)]
pub struct CircularBuffer {
    data: Box<[u8]>,
    capacity: usize,
    write_pos: usize,
    len: usize,
}

impl CircularBuffer {
    /// Creates a new circular buffer with the given capacity.
    ///
    /// # Arguments
    /// * `capacity` - Maximum number of bytes the buffer can hold
    ///
    /// # Panics
    /// Panics if capacity is 0
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "CircularBuffer capacity must be > 0");
        Self {
            data: vec![0u8; capacity].into_boxed_slice(),
            capacity,
            write_pos: 0,
            len: 0,
        }
    }

    /// Creates a new circular buffer with the default capacity (10 MB).
    pub fn with_default_capacity() -> Self {
        Self::new(DEFAULT_HISTORY_CAPACITY)
    }

    /// Appends bytes to the buffer, overwriting oldest data if necessary.
    ///
    /// # Arguments
    /// * `bytes` - The bytes to append
    pub fn push(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }

        // If input is larger than capacity, only keep the last `capacity` bytes
        let bytes = if bytes.len() > self.capacity {
            &bytes[bytes.len() - self.capacity..]
        } else {
            bytes
        };

        for &byte in bytes {
            self.data[self.write_pos] = byte;
            self.write_pos = (self.write_pos + 1) % self.capacity;
            if self.len < self.capacity {
                self.len += 1;
            }
        }
    }

    /// Returns the current contents as a Vec<u8>.
    ///
    /// The returned vector contains the data in chronological order,
    /// handling the wrap-around correctly.
    pub fn to_vec(&self) -> Vec<u8> {
        if self.len == 0 {
            return Vec::new();
        }

        let mut result = Vec::with_capacity(self.len);

        if self.len < self.capacity {
            // Buffer hasn't wrapped yet - data is at the start
            result.extend_from_slice(&self.data[..self.len]);
        } else {
            // Buffer has wrapped - oldest data starts at write_pos
            result.extend_from_slice(&self.data[self.write_pos..]);
            result.extend_from_slice(&self.data[..self.write_pos]);
        }

        result
    }

    /// Returns the number of bytes currently in the buffer.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns true if the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns the capacity of the buffer.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Clears the buffer.
    pub fn clear(&mut self) {
        self.write_pos = 0;
        self.len = 0;
    }
}

impl Clone for CircularBuffer {
    fn clone(&self) -> Self {
        Self {
            data: self.data.clone(),
            capacity: self.capacity,
            write_pos: self.write_pos,
            len: self.len,
        }
    }
}

// Re-export from constants
pub use crate::constants::COMPRESSION_THRESHOLD;

/// Compress data using zstd compression.
/// Returns the compressed bytes, or an error if compression fails.
pub fn compress_history(data: &[u8]) -> Result<Vec<u8>, std::io::Error> {
    zstd::encode_all(std::io::Cursor::new(data), 3) // compression level 3 (default)
}

/// Decompress zstd-compressed data.
/// Returns the decompressed bytes, or an error if decompression fails.
pub fn decompress_history(data: &[u8]) -> Result<Vec<u8>, std::io::Error> {
    zstd::decode_all(std::io::Cursor::new(data))
}

/// Check if the given data size exceeds the compression threshold.
pub fn should_compress(data_len: usize) -> bool {
    data_len > COMPRESSION_THRESHOLD
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_buffer() {
        let buf = CircularBuffer::new(100);
        assert!(buf.is_empty());
        assert_eq!(buf.len(), 0);
        assert_eq!(buf.to_vec(), Vec::<u8>::new());
    }

    #[test]
    fn test_push_within_capacity() {
        let mut buf = CircularBuffer::new(100);
        buf.push(b"hello");
        assert_eq!(buf.len(), 5);
        assert_eq!(buf.to_vec(), b"hello".to_vec());
    }

    #[test]
    fn test_push_exactly_capacity() {
        let mut buf = CircularBuffer::new(5);
        buf.push(b"hello");
        assert_eq!(buf.len(), 5);
        assert_eq!(buf.to_vec(), b"hello".to_vec());
    }

    #[test]
    fn test_push_wrapping() {
        let mut buf = CircularBuffer::new(5);
        buf.push(b"hello");
        buf.push(b"XY");
        // Buffer should now contain "lloXY" (oldest "he" overwritten)
        assert_eq!(buf.len(), 5);
        assert_eq!(buf.to_vec(), b"lloXY".to_vec());
    }

    #[test]
    fn test_push_larger_than_capacity() {
        let mut buf = CircularBuffer::new(5);
        buf.push(b"hello world");
        // Only last 5 bytes should be kept
        assert_eq!(buf.len(), 5);
        assert_eq!(buf.to_vec(), b"world".to_vec());
    }

    #[test]
    fn test_multiple_pushes() {
        let mut buf = CircularBuffer::new(10);
        buf.push(b"aaa");
        buf.push(b"bbb");
        buf.push(b"ccc");
        assert_eq!(buf.len(), 9);
        assert_eq!(buf.to_vec(), b"aaabbbccc".to_vec());
    }

    #[test]
    fn test_multiple_pushes_with_wrap() {
        let mut buf = CircularBuffer::new(10);
        buf.push(b"12345");
        buf.push(b"67890");
        buf.push(b"abc");
        // Buffer wraps: oldest "123" overwritten
        assert_eq!(buf.len(), 10);
        assert_eq!(buf.to_vec(), b"4567890abc".to_vec());
    }

    #[test]
    fn test_clear() {
        let mut buf = CircularBuffer::new(100);
        buf.push(b"hello");
        buf.clear();
        assert!(buf.is_empty());
        assert_eq!(buf.len(), 0);
        assert_eq!(buf.to_vec(), Vec::<u8>::new());
    }

    #[test]
    fn test_push_empty() {
        let mut buf = CircularBuffer::new(100);
        buf.push(b"hello");
        buf.push(b"");
        assert_eq!(buf.len(), 5);
        assert_eq!(buf.to_vec(), b"hello".to_vec());
    }

    #[test]
    fn test_clone() {
        let mut buf = CircularBuffer::new(10);
        buf.push(b"hello");
        let cloned = buf.clone();
        assert_eq!(buf.to_vec(), cloned.to_vec());
        assert_eq!(buf.len(), cloned.len());
    }

    #[test]
    #[should_panic(expected = "capacity must be > 0")]
    fn test_zero_capacity_panics() {
        CircularBuffer::new(0);
    }

    #[test]
    fn test_compression_threshold_is_1mb() {
        assert_eq!(COMPRESSION_THRESHOLD, 1024 * 1024);
    }

    #[test]
    fn test_should_compress_below_threshold() {
        assert!(!should_compress(100), "Small data should not be compressed");
        assert!(
            !should_compress(COMPRESSION_THRESHOLD),
            "Exactly threshold should not compress"
        );
    }

    #[test]
    fn test_should_compress_above_threshold() {
        assert!(
            should_compress(COMPRESSION_THRESHOLD + 1),
            "Above threshold should compress"
        );
    }

    #[test]
    fn test_compress_and_decompress_roundtrip() {
        let original = b"Hello, World! This is test data for compression.";
        let compressed = compress_history(original).expect("Compression should succeed");
        let decompressed = decompress_history(&compressed).expect("Decompression should succeed");
        assert_eq!(
            decompressed,
            original.to_vec(),
            "Decompressed data should match original"
        );
    }

    #[test]
    fn test_compress_empty_data() {
        let original = b"";
        let compressed =
            compress_history(original).expect("Compression of empty data should succeed");
        let decompressed = decompress_history(&compressed).expect("Decompression should succeed");
        assert_eq!(decompressed, original.to_vec());
    }

    #[test]
    fn test_compress_large_repetitive_data_is_smaller() {
        let original: Vec<u8> = "AAAA".repeat(100_000).into_bytes();
        let compressed = compress_history(&original).expect("Compression should succeed");
        assert!(
            compressed.len() < original.len(),
            "Compressed size ({}) should be less than original ({})",
            compressed.len(),
            original.len()
        );

        let decompressed = decompress_history(&compressed).expect("Decompression should succeed");
        assert_eq!(decompressed, original);
    }

    #[test]
    fn test_decompress_invalid_data_returns_error() {
        let garbage = b"this is not valid zstd data";
        let result = decompress_history(garbage);
        assert!(
            result.is_err(),
            "Decompressing invalid data should return an error"
        );
    }

    #[test]
    fn test_compress_history_from_circular_buffer() {
        let mut buf = CircularBuffer::new(1024);
        let data = "Terminal output data for compression test\n".repeat(20);
        buf.push(data.as_bytes());

        let history_data = buf.to_vec();
        let compressed = compress_history(&history_data).expect("Should compress buffer data");
        let decompressed = decompress_history(&compressed).expect("Should decompress");
        assert_eq!(
            decompressed, history_data,
            "Roundtrip through compress/decompress should preserve buffer content"
        );
    }
}
