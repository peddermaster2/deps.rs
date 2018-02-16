use std::io::{Error as IoError};
use std::str::Utf8Error;
use std::string::FromUtf8Error;
use std::num::ParseIntError;
use hyper::{Error as HyperError, StatusCode};
use hyper::error::UriError;

#[derive(Debug, Fail)]
pub enum Error {
    #[fail(display = "failed to convert to utf8")]
    Utf8(Utf8Error),
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
    Hyper(HyperError),
    #[fail(display = "unexpected http status code {}", code)]
    UnexpectedStatusCode { code: StatusCode },
    #[fail(display = "i/o operation failed")]
    Io(IoError)
}
