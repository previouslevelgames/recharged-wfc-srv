//! GameSpy's text messages: `\key\value\key\value\final\`. The first key
//! names the command; values are empty or text without `\`.

use std::fmt;

const FINAL: &[u8] = b"\\final\\";

#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pairs: Vec<(String, String)>,
}

impl Message {
    pub fn new(command: &str, value: impl fmt::Display) -> Self {
        Self {
            pairs: vec![(command.to_owned(), value.to_string())],
        }
    }

    pub fn with(mut self, key: &str, value: impl fmt::Display) -> Self {
        self.pairs.push((key.to_owned(), value.to_string()));
        self
    }

    pub fn command(&self) -> &str {
        &self.pairs[0].0
    }

    /// The value of `key`; the command's own value under its name.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.pairs
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    /// Parses one message, without its `\final\`.
    fn parse(bytes: &[u8]) -> Option<Self> {
        let text = String::from_utf8_lossy(bytes);
        let mut parts = text.strip_prefix('\\')?.split('\\');
        let mut pairs = Vec::new();
        while let Some(key) = parts.next() {
            if key.is_empty() {
                return None;
            }
            pairs.push((key.to_owned(), parts.next().unwrap_or_default().to_owned()));
        }
        (!pairs.is_empty()).then_some(Self { pairs })
    }
}

impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (key, value) in &self.pairs {
            write!(f, "\\{key}\\{value}")?;
        }
        f.write_str("\\final\\")
    }
}

/// Takes the complete messages off the front of `buffer`, leaving a partial
/// one. `Err` for a message that does not parse.
pub fn split(buffer: &mut Vec<u8>) -> Vec<Result<Message, ()>> {
    let mut messages = Vec::new();
    while let Some(end) = buffer
        .windows(FINAL.len())
        .position(|window| window == FINAL)
    {
        messages.push(Message::parse(&buffer[..end]).ok_or(()));
        buffer.drain(..end + FINAL.len());
    }
    messages
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_pairs_then_final() {
        let message = Message::new("lc", 1)
            .with("challenge", "ABCDEFGHIJ")
            .with("id", 1);
        assert_eq!(
            message.to_string(),
            r"\lc\1\challenge\ABCDEFGHIJ\id\1\final\"
        );
    }

    #[test]
    fn splits_complete_messages_and_keeps_the_rest() {
        let mut buffer = br"\login\\challenge\xyz\id\1\final\\ka\\final\\status\1\stat".to_vec();
        let messages: Vec<_> = split(&mut buffer).into_iter().map(Result::unwrap).collect();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].command(), "login");
        assert_eq!(messages[0].get("login"), Some(""));
        assert_eq!(messages[0].get("challenge"), Some("xyz"));
        assert_eq!(messages[0].get("id"), Some("1"));
        assert_eq!(messages[1].command(), "ka");
        assert_eq!(buffer, br"\status\1\stat");
    }

    #[test]
    fn refuses_what_is_not_a_message() {
        for bad in [
            &b"login\\x\\final\\"[..],
            b"\\final\\",
            b"\\a\\\\\\b\\final\\",
        ] {
            let mut buffer = bad.to_vec();
            assert_eq!(split(&mut buffer), [Err(())], "{bad:?}");
        }
    }
}
