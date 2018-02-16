#![feature(nll)]

#[macro_use] extern crate failure;
#[macro_use] extern crate futures;
extern crate hyper;
extern crate tokio_service;

use std::collections::VecDeque;
use std::io::Cursor;
use std::mem;

use futures::{Async, Future, Poll, Stream};
use futures::stream::{iter_ok, IterOk, Concat2};
use hyper::{Body, Client};
use hyper::client::{Connect, FutureResponse};
use tokio_service::Service;

mod error;
mod parser;

pub use self::error::Error;
use self::parser::{Parser, ParseResult};
use self::parser::parse;

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

#[derive(Debug)]
pub struct LsRemote<C: Connect>  {
    client: Client<C>
}

impl<C: Connect> LsRemote<C> {
    pub fn new(client: Client<C>) -> LsRemote<C> {
        LsRemote { client }
    }

    pub fn all_refs(&self, mut req: LsRemoteRequest) -> LsRemoteStream {
        req.https_clone_url.push_str("/info/refs?service=git-upload-pack");
        let uri = match req.https_clone_url.parse() {
            Ok(uri) => uri,
            Err(err) => {
                return LsRemoteStream(FutureState::Error(Some(Error::Uri(err))));
            }
        };
        LsRemoteStream(FutureState::Request(self.client.get(uri)))
    }
}

impl<C: Connect> Service for LsRemote<C> {
    type Request = LsRemoteRequest;
    type Response = Option<ObjectId>;
    type Error = Error;
    type Future = LsRemoteFuture;

    fn call(&self, req: LsRemoteRequest) -> Self::Future {
        LsRemoteFuture(self.all_refs(req))
    }
}

pub struct LsRemoteRequest {
    pub https_clone_url: String
}

pub struct LsRemoteFuture(LsRemoteStream);

impl Future for LsRemoteFuture {
    type Item = Option<ObjectId>;
    type Error = Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        match try_ready!(self.0.poll()) {
            None => Ok(Async::Ready(None)),
            Some(r) => {
                if r.name == "HEAD" {
                    Ok(Async::Ready(Some(r.oid)))
                } else {
                    Ok(Async::Ready(None))
                }
            }
        }
    }
}

pub struct LsRemoteStream(FutureState);

impl Stream for LsRemoteStream {
    type Item = Ref;
    type Error = Error;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        match self.0 {
            FutureState::Request(ref mut future) => {
                match future.poll() {
                    Err(err) => Err(Error::Hyper(err)),
                    Ok(Async::NotReady) => {
                        Ok(Async::NotReady)
                    },
                    Ok(Async::Ready(response)) => {
                        if response.status().is_success() {
                            let parser = Parser::new();
                            let body = response.body();
                            let buf = Vec::new();
                            let refs = VecDeque::new();
                            self.0 = FutureState::Streaming(parser, body, buf, refs);
                            self.poll()
                        } else {
                            Err(Error::UnexpectedStatusCode { code: response.status() })
                        }
                    }
                }
            }
            FutureState::Streaming(ref mut parser, ref mut body, ref mut buf, ref mut refs) => {
                if refs.len() > 0 {
                    Ok(Async::Ready(refs.pop_front()))
                } else {
                    match parser.update(buf) {
                        ParseResult::Error(err) => Err(err.into()),
                        ParseResult::End => Ok(Async::Ready(None)),
                        ParseResult::Yield(r) => {
                            refs.push_back(r);
                            self.poll()
                        },
                        ParseResult::Incomplete => {
                            match try_ready!(body.poll().map_err(Error::Hyper)) {
                                Some(chunk) => {
                                    buf.extend_from_slice(chunk.as_ref());
                                    self.poll()
                                },
                                None => {
                                    parser.finalize()?;
                                    Ok(Async::Ready(None))
                                }
                            }
                        }
                    }
                }
            },
            FutureState::Error(ref mut err_opt) => Err(err_opt.take().unwrap())
        }
    }
}

#[derive(Debug)]
enum FutureState {
    Error(Option<Error>),
    Request(FutureResponse),
    Streaming(Parser, Body, Vec<u8>, VecDeque<Ref>)
} 
