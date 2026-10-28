//! Injectable HTTP transport. Requests never log authentication headers or bodies.
use crate::{Error, Result};
use futures::future::BoxFuture;
use reqwest::{Client, Method, header::HeaderMap};

#[derive(Clone, Debug)]
pub struct HTTPRequest {
    pub method: Method,
    pub url: String,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl HTTPRequest {
    pub fn new(method: Method, url: impl Into<String>) -> Self {
        Self {
            method,
            url: url.into(),
            headers: HeaderMap::new(),
            body: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct HTTPResponse {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl HTTPResponse {
    pub fn require_success(self, request: &HTTPRequest) -> Result<Self> {
        if (200..300).contains(&self.status) {
            return Ok(self);
        }
        Err(Error::Http {
            method: request.method.to_string(),
            url: request.url.clone(),
            status: self.status,
            body: String::from_utf8_lossy(&self.body).trim().to_owned(),
        })
    }
}

pub trait HTTPClientProtocol: Send + Sync {
    fn send(&self, request: HTTPRequest) -> BoxFuture<'_, Result<HTTPResponse>>;
}

#[derive(Clone)]
pub struct HTTPClient(pub Client);

impl HTTPClient {
    pub fn new() -> Result<Self> {
        Ok(Self(plume_core::client()?))
    }
}

impl HTTPClientProtocol for HTTPClient {
    fn send(&self, request: HTTPRequest) -> BoxFuture<'_, Result<HTTPResponse>> {
        Box::pin(async move {
            let response = self
                .0
                .request(request.method, &request.url)
                .headers(request.headers)
                .body(request.body)
                .send()
                .await?;
            Ok(HTTPResponse {
                status: response.status().as_u16(),
                headers: response.headers().clone(),
                body: response.bytes().await?.to_vec(),
            })
        })
    }
}
