#[macro_use] extern crate failure;
extern crate futures;
extern crate hyper;
extern crate itertools;
extern crate tokio_service;

use std::io::{self, Cursor, Read};
use std::mem;
use std::num::ParseIntError;
use std::str;
use std::string::FromUtf8Error;

use futures::{Async, Future, Poll, Stream};
use futures::stream::Concat2;
use hyper::{Body, Client, StatusCode};
use hyper::client::{Connect, FutureResponse};
use hyper::error::UriError;
use itertools::put_back;
use tokio_service::Service;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ObjectId(String);

impl AsRef<str> for ObjectId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Ref {
    pub name: String,
    pub oid: ObjectId
}

#[derive(Debug, Fail)]
pub enum Error {
    #[fail(display = "failed to convert to utf8")]
    Utf8(str::Utf8Error),
    #[fail(display = "failed to convert to utf8")]
    FromUtf8(FromUtf8Error),
    #[fail(display = "failed to parse into integer")]
    ParseInt(ParseIntError, String),
    #[fail(display = "invalid line length")]
    InvalidLineLength,
    #[fail(display = "invalid line syntax")]
    InvalidLine(String),
    #[fail(display = "unexpected end of payload")]
    UnexpectedEndOfPayload,
    #[fail(display = "failed to parse uri")]
    Uri(UriError),
    #[fail(display = "http error")]
    Hyper(hyper::Error),
    #[fail(display = "unexpected http status code {}", code)]
    UnexpectedStatusCode { code: StatusCode },
    #[fail(display = "i/o operation failed")]
    Io(io::Error)
}

#[derive(Debug)]
pub struct LsRemote<C: Connect>  {
    client: Client<C>
}

impl<C: Connect> LsRemote<C> {
    pub fn new(client: Client<C>) -> LsRemote<C> {
        LsRemote { client }
    }
}

impl<C: Connect> Service for LsRemote<C> {
    type Request = LsRemoteRequest;
    type Response = Vec<Ref>;
    type Error = Error;
    type Future = LsRemoteFuture;

    fn call(&self, mut req: LsRemoteRequest) -> LsRemoteFuture {
        req.https_clone_url.push_str("/info/refs?service=git-upload-pack");
        let uri = match req.https_clone_url.parse() {
            Ok(uri) => uri,
            Err(err) => {
                return LsRemoteFuture(FutureState::Error(Error::Uri(err)));
            }
        };
        LsRemoteFuture(FutureState::Request(self.client.get(uri)))
    }
}

pub struct LsRemoteRequest {
    pub https_clone_url: String
}

pub struct LsRemoteFuture(FutureState);

impl Future for LsRemoteFuture {
    type Item = Vec<Ref>;
    type Error = Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        match mem::replace(&mut self.0, FutureState::Swapping) {
            FutureState::Request(mut future) => {
                match future.poll() {
                    Err(err) => Err(Error::Hyper(err)),
                    Ok(Async::NotReady) => {
                        self.0 = FutureState::Request(future);
                        Ok(Async::NotReady)
                    },
                    Ok(Async::Ready(response)) => {
                        if response.status().is_success() {
                            self.0 = FutureState::Streaming(response.body().concat2());
                            self.poll()
                        } else {
                            Err(Error::UnexpectedStatusCode { code: response.status() })
                        }
                    }
                }
            }
            FutureState::Streaming(mut future) => {
                match future.poll() {
                    Err(err) => Err(Error::Hyper(err)),
                    Ok(Async::NotReady) => {
                        self.0 = FutureState::Streaming(future);
                        Ok(Async::NotReady)
                    },
                    Ok(Async::Ready(body)) => {
                        let mut cursor = Cursor::new(&body);
                        let refs = parse_body(&mut cursor)?;
                        Ok(Async::Ready(refs))
                    }
                }
            },
            FutureState::Error(err) => Err(err),
            FutureState::Swapping => unreachable!()
        }
    }
}

#[derive(Debug)]
enum FutureState {
    Error(Error),
    Request(FutureResponse),
    Streaming(Concat2<Body>),
    Swapping
} 

fn parse_body<R: Read>(body: &mut R) -> Result<Vec<Ref>, Error> {
    let mut lines = Vec::new();
    loop {
        let mut len_bytes = [0u8; 4];
        if let Err(err) = body.read_exact(&mut len_bytes) {
            if err.kind() == io::ErrorKind::UnexpectedEof {
                break;
            } else {
                return Err(Error::Io(err));
            }
        }
        let len_str = str::from_utf8(&len_bytes).map_err(Error::Utf8)?;
        let len = usize::from_str_radix(len_str, 16)
            .map_err(|err| Error::ParseInt(err, len_str.into()))?;
        if len == 0 || len == 4 {
            continue;
        } else if len < 4 {
            return Err(Error::InvalidLineLength);
        } else {
            let mut line_bytes = vec![0; len - 4];
            body.read_exact(&mut line_bytes).map_err(Error::Io)?;
            let mut line_string = String::from_utf8(line_bytes).map_err(Error::FromUtf8)?;
            if let Some(last_char) = line_string.pop() {
                if last_char != '\n' {
                    line_string.push(last_char);
                }
            }
            lines.push(line_string);
        }
    }
    parse_lines(lines)
}


fn parse_lines(lines: Vec<String>) -> Result<Vec<Ref>, Error> {
    let mut iter = put_back(lines);

    if let Some(first_line) = iter.next() {
        if first_line != "# service=git-upload-pack" {
            return Err(Error::InvalidLine(first_line));
        }
    } else {
        return Err(Error::UnexpectedEndOfPayload)
    }

    if let Some(mut second_line) = iter.next() {
        if let Some(nul_index) = second_line.find('\0') {
            // non empty list
            second_line.truncate(nul_index);
            iter.put_back(second_line);
        } else {
            // empty list
            return Ok(Vec::new());
        }
    } else {
        return Err(Error::UnexpectedEndOfPayload)
    }

    iter.map(|mut line| {
        if let Some(space_index) = line.find(' ') {
            let mut name = line.split_off(space_index + 1);
            line.pop(); // remove trailing space
            Ok(Ref { name, oid: ObjectId(line) })
        } else {
            Err(Error::InvalidLine(line))
        }
    }).collect()
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::parse_body;

    static PAYLOAD: &[u8] = include_bytes!("../test_fixtures/payload");

    #[test]
    fn parse_payload() {
        let mut cursor = Cursor::new(PAYLOAD);
        let refs = parse_body(&mut cursor).unwrap();

        assert_eq!(refs[0].name, "HEAD");
        assert_eq!(refs[0].oid.as_ref(), "990fa3a054f979b66989c79df21b8c71d8eb946f");
    }
}
