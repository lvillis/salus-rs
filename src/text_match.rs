#[derive(Debug)]
pub(crate) struct TextMatcher {
    needles: Vec<Vec<u8>>,
    matched: Vec<bool>,
    matched_count: usize,
}

impl TextMatcher {
    pub(crate) fn new(needles: &[String]) -> Self {
        let needles = needles
            .iter()
            .map(|needle| needle.as_bytes().to_vec())
            .collect::<Vec<_>>();
        let matched = needles
            .iter()
            .map(|needle| needle.is_empty())
            .collect::<Vec<_>>();
        let matched_count = matched.iter().filter(|matched| **matched).count();

        Self {
            needles,
            matched,
            matched_count,
        }
    }

    pub(crate) fn observe_appended(&mut self, haystack: &[u8], previous_len: usize) {
        let previous_len = previous_len.min(haystack.len());

        for (index, needle) in self.needles.iter().enumerate() {
            if self.matched[index] {
                continue;
            }

            let start = previous_len.saturating_sub(needle.len().saturating_sub(1));
            if contains_slice(&haystack[start..], needle) {
                self.matched[index] = true;
                self.matched_count += 1;
            }
        }
    }

    pub(crate) fn all_matched(&self) -> bool {
        self.matched_count == self.needles.len()
    }

    pub(crate) fn any_matched(&self) -> bool {
        self.matched_count > 0
    }
}

pub(crate) fn contains_bytes(bytes: &[u8], needle: &str) -> bool {
    contains_slice(bytes, needle.as_bytes())
}

fn contains_slice(bytes: &[u8], needle: &[u8]) -> bool {
    needle.is_empty() || bytes.windows(needle.len()).any(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::{TextMatcher, contains_bytes};

    #[test]
    fn matcher_finds_text_across_append_boundary() {
        let mut matcher = TextMatcher::new(&["ready".to_string()]);
        let mut bytes = b"rea".to_vec();
        matcher.observe_appended(&bytes, 0);

        assert!(!matcher.all_matched());

        let previous_len = bytes.len();
        bytes.extend_from_slice(b"dy");
        matcher.observe_appended(&bytes, previous_len);

        assert!(matcher.all_matched());
    }

    #[test]
    fn matcher_tracks_any_match() {
        let mut matcher = TextMatcher::new(&["bad".to_string(), "error".to_string()]);
        matcher.observe_appended(b"all bad", 0);

        assert!(matcher.any_matched());
        assert!(!matcher.all_matched());
    }

    #[test]
    fn contains_bytes_uses_raw_bytes() {
        assert!(contains_bytes(b"\xffready", "ready"));
        assert!(!contains_bytes(b"\xff", "\u{FFFD}"));
    }
}
