use std::io::Read;
use std::str::from_utf8;

use super::{Error, ObjectId, Ref};

#[derive(Debug)]
pub enum ParseResult<T, E> {
    Error(E),
    Yield(T),
    Incomplete,
    End
}

macro_rules! try_parse {
    ($expression:expr) => (match $expression {
        Ok(value) => value,
        Err(err) => {
            return ParseResult::Error(err);
        }
    })
}

#[derive(Debug)]
enum ParseState {
    AwaitingLength,
    AwaitingLineOfLength(usize)
}

#[derive(Debug)]
pub struct Parser {
    state: ParseState,
    line_idx: usize
}

impl Parser {
    pub fn new() -> Parser {
        Parser {
            state: ParseState::AwaitingLength,
            line_idx: 0
        }
    }

    pub fn update(&mut self, data: &mut Vec<u8>) -> ParseResult<Ref, Error> {
        match self.state {
            ParseState::AwaitingLength => {
                if data.len() < 4 {
                    return ParseResult::Incomplete;
                }
                let len_bytes = data.drain(..4).collect::<Vec<_>>();
                let len_str = try_parse!(from_utf8(&len_bytes).map_err(Error::Utf8));
                let len = try_parse!(usize::from_str_radix(len_str, 16)
                    .map_err(|err| Error::ParseInt(err, len_str.into())));
                if len == 0 || len == 4 {
                    return self.update(data);
                } else if len < 4 {
                    return ParseResult::Error(Error::InvalidLineLength);
                } else {
                    self.state = ParseState::AwaitingLineOfLength(len - 4);
                    return self.update(data);
                }
            },
            ParseState::AwaitingLineOfLength(len) => {
                if data.len() < len {
                    return ParseResult::Incomplete;
                }
                let mut line_bytes = data.drain(..len).collect::<Vec<_>>();
                let mut line_string = try_parse!(String::from_utf8(line_bytes).map_err(Error::FromUtf8));
                if let Some(last_char) = line_string.pop() {
                    if last_char != '\n' {
                        line_string.push(last_char);
                    }
                }
                self.parse_line(line_string)
            }
        }
    }

    pub fn finalize(&mut self) -> Result<(), Error> {
        if let ParseState::AwaitingLength = self.state {
            if self.line_idx > 1 {
                return Ok(());
            }
        }
        return Err(Error::UnexpectedEndOfPayload);
    }

    fn parse_line(&mut self, mut line: String) -> ParseResult<Ref, Error> {
        if self.line_idx == 0 {
            if line != "# service=git-upload-pack" {
                ParseResult::Error(Error::InvalidLine(line))
            } else {
                self.line_idx += 1;
                self.state = ParseState::AwaitingLength;
                ParseResult::Incomplete
            }
        } else if self.line_idx == 1 {
            if let Some(nul_index) = line.find('\0') {
                // non empty list
                line.truncate(nul_index);
                self.line_idx +=1;
                self.parse_line(line)
            } else {
                // empty list
                ParseResult::End
            }
        } else {
            if let Some(space_index) = line.find(' ') {
                let mut name = line.split_off(space_index + 1);
                line.pop(); // remove trailing space
                self.line_idx += 1;
                self.state = ParseState::AwaitingLength;
                ParseResult::Yield(Ref { name, oid: ObjectId(line) })
            } else {
                ParseResult::Error(Error::InvalidLine(line))
            }
        }
    }
}

pub fn parse<R: Read>(body: &mut R) -> Result<Vec<Ref>, Error> {
    let mut parser = Parser::new();
    let mut data = Vec::new();
    let mut buf = [0u8; 128];
    let mut refs = Vec::new();
    loop {
        let len = body.read(&mut buf).map_err(Error::Io)?;
        if len == 0 {
            parser.finalize()?;
            return Ok(refs);
        }
        data.extend(buf[..len].iter());
        loop {
            match parser.update(&mut data) {
                ParseResult::Error(err) => {
                    return Err(err);
                },
                ParseResult::Yield(r) => {
                    refs.push(r);
                },
                ParseResult::Incomplete => {
                    break;
                },
                ParseResult::End => {
                    return Ok(refs);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::parse;

    static PAYLOAD: &[u8] = include_bytes!("../test_fixtures/payload");

    #[test]
    fn parse_payload() {
        let mut cursor = Cursor::new(PAYLOAD);
        let refs = parse(&mut cursor).unwrap();

        assert_eq!(refs.len(), 53);
        assert_eq!(refs[0].name, "HEAD");
        assert_eq!(refs[0].oid.as_ref(), "990fa3a054f979b66989c79df21b8c71d8eb946f");
    }
}
