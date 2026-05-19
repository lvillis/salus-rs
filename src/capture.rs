#[derive(Clone, Debug, Default)]
pub(crate) struct CaptureBuffer {
    bytes: Vec<u8>,
    // Incomplete means the captured bytes cannot prove the full stream contents.
    // This can be caused by dropped bytes, a declared size beyond the limit, or an open stream.
    incomplete: bool,
}

impl CaptureBuffer {
    pub(crate) fn append_limited(&mut self, chunk: &[u8], limit: usize) -> usize {
        let previous_len = self.bytes.len();

        if self.bytes.len() < limit {
            let remaining = limit - self.bytes.len();
            if chunk.len() > remaining {
                self.incomplete = true;
            }
            self.bytes
                .extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        } else if !chunk.is_empty() {
            self.incomplete = true;
        }

        previous_len
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub(crate) fn len(&self) -> usize {
        self.bytes.len()
    }

    pub(crate) fn is_incomplete(&self) -> bool {
        self.incomplete
    }

    pub(crate) fn mark_incomplete(&mut self) {
        self.incomplete = true;
    }
}

#[cfg(test)]
mod tests {
    use super::CaptureBuffer;

    #[test]
    fn append_limited_keeps_data_within_limit() {
        let mut buffer = CaptureBuffer::default();

        let previous_len = buffer.append_limited(b"abcdef", 3);

        assert_eq!(previous_len, 0);
        assert_eq!(buffer.bytes(), b"abc");
        assert!(buffer.is_incomplete());
    }

    #[test]
    fn append_limited_marks_later_data_after_full_limit() {
        let mut buffer = CaptureBuffer::default();
        buffer.append_limited(b"abc", 3);

        let previous_len = buffer.append_limited(b"d", 3);

        assert_eq!(previous_len, 3);
        assert_eq!(buffer.bytes(), b"abc");
        assert!(buffer.is_incomplete());
    }

    #[test]
    fn append_limited_keeps_exact_limit_complete() {
        let mut buffer = CaptureBuffer::default();

        buffer.append_limited(b"abc", 3);

        assert_eq!(buffer.bytes(), b"abc");
        assert!(!buffer.is_incomplete());
    }
}
