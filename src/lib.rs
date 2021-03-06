//! jsonrpc http server.
//! 
//! ```no_run
//! extern crate jsonrpc_core;
//! extern crate jsonrpc_http_server;
//! 
//! use std::sync::Arc;
//! use jsonrpc_core::*;
//! use jsonrpc_http_server::*;
//! 
//! struct SayHello;
//! impl MethodCommand for SayHello {
//! 	fn execute(&self, _params: Params) -> Result<Value, Error> {
//! 		Ok(Value::String("hello".to_string()))
//! 	}
//! }
//! 
//! fn main() {
//! 	let io = IoHandler::new();
//! 	io.add_method("say_hello", SayHello);
//! 	let _server = Server::start(&"127.0.0.1:3030".parse().unwrap(), Arc::new(io), Some(AccessControlAllowOrigin::Null));
//! }
//! ```

extern crate hyper;
extern crate unicase;
extern crate jsonrpc_core as jsonrpc;

use std::ops::Deref;
use std::sync::{Arc, Mutex};
use std::io::{Read, Write};
use std::net::SocketAddr;
use hyper::header::{Headers, Allow, ContentType, AccessControlAllowHeaders};
use hyper::method::Method;
use hyper::server::{Request, Response};
use hyper::{Next, Encoder, Decoder};
use hyper::net::HttpStream;
use unicase::UniCase;
use self::jsonrpc::{IoHandler};

pub use hyper::header::AccessControlAllowOrigin;

pub type ServerResult = Result<Server, RpcServerError>;

/// RPC Server startup error
#[derive(Debug)]
pub enum RpcServerError {
	IoError(std::io::Error),
	Other(hyper::error::Error),
}

impl From<hyper::error::Error> for RpcServerError {
	fn from(err: hyper::error::Error) -> Self {
		match err {
			hyper::error::Error::Io(e) => RpcServerError::IoError(e),
			e => RpcServerError::Other(e)
		}
	}
}

/// PanicHandling function
pub struct PanicHandler {
	pub handler: Arc<Mutex<Option<Box<Fn() -> () + Send + 'static>>>>
}

/// jsonrpc http request handler.
pub struct ServerHandler {
	panic_handler: PanicHandler,
	jsonrpc_handler: Arc<IoHandler>,
	cors_domain: Option<AccessControlAllowOrigin>,
	request: String,
	response: Option<String>,
	write_pos: usize,
}

impl Drop for ServerHandler {
	fn drop(&mut self) {
		if ::std::thread::panicking() {
			let handler = self.panic_handler.handler.lock().unwrap();
			if let Some(ref h) = *handler.deref() {
				h();
			}
		}
	}
}

impl ServerHandler {
	/// Create new request handler.
	pub fn new(jsonrpc_handler: Arc<IoHandler>, cors_domain: Option<AccessControlAllowOrigin>, panic_handler: PanicHandler) -> Self {
		ServerHandler {
			panic_handler: panic_handler,
			jsonrpc_handler: jsonrpc_handler,
			cors_domain: cors_domain,
			request: String::new(),
			response: None,
			write_pos: 0,
		}
	}

	fn response_headers(&self) -> Headers {
		let mut headers = Headers::new();
		headers.set(
			Allow(vec![
				Method::Options, Method::Post
			])
		);
		headers.set(ContentType::json());
		headers.set(
			AccessControlAllowHeaders(vec![
				UniCase("origin".to_owned()),
				UniCase("content-type".to_owned()),
				UniCase("accept".to_owned()),
			])
		);

		if let Some(ref cors_domain) = self.cors_domain {
			headers.set(cors_domain.clone());
		}
		headers
	}
}

impl hyper::server::Handler<HttpStream> for ServerHandler {
	fn on_request(&mut self, request: Request) -> Next {
		match *request.method() {
			Method::Options => {
				self.response = Some(String::new());
				Next::write()
			},
			Method::Post => Next::read(),
			_ => Next::write(),
		}
	}

	/// This event occurs each time the `Request` is ready to be read from.
	fn on_request_readable(&mut self, decoder: &mut Decoder<HttpStream>) -> Next {
		match decoder.read_to_string(&mut self.request) {
			Ok(0) => {
				self.response = self.jsonrpc_handler.handle_request(&self.request);
				match self.response {
					Some(ref mut r) => r.push('\n'),
					_ => ()
				}
				Next::write()
			}
			Ok(_) => {
				Next::read()
			}
			Err(e) => match e.kind() {
				::std::io::ErrorKind::WouldBlock => Next::read(),
				_ => {
					//trace!("Read error: {}", e);
					Next::end()
				}
			}
		}
	}

		/// This event occurs after the first time this handled signals `Next::write()`.
	fn on_response(&mut self, response: &mut Response) -> Next {
		*response.headers_mut() = self.response_headers();
		if self.response.is_none() {
			response.set_status(hyper::status::StatusCode::MethodNotAllowed);
		}
		Next::write()
	}

		/// This event occurs each time the `Response` is ready to be written to.
		fn on_response_writable(&mut self, encoder: &mut Encoder<HttpStream>) -> Next {
		if let Some(ref response) = self.response {
			let bytes = response.as_bytes();
			if bytes.len() == self.write_pos {
				Next::end()
			} else {
				match encoder.write(&bytes[self.write_pos ..]) {
					Ok(0) => {
						Next::write()
					}
					Ok(bytes) => {
						self.write_pos += bytes;
						Next::write()
					}
					Err(e) => match e.kind() {
						::std::io::ErrorKind::WouldBlock => Next::write(),
						_ => {
							//trace!("Write error: {}", e);
							Next::end()
						}
					}
				}
			}
		} else {
			Next::end()
		}
	}
}

/// jsonrpc http server.
///
/// ```no_run
/// extern crate jsonrpc_core;
/// extern crate jsonrpc_http_server;
///
/// use std::sync::Arc;
/// use jsonrpc_core::*;
/// use jsonrpc_http_server::*;
///
/// struct SayHello;
/// impl MethodCommand for SayHello {
/// 	fn execute(&self, _params: Params) -> Result<Value, Error> {
/// 		Ok(Value::String("hello".to_string()))
/// 	}
/// }
///
/// fn main() {
/// 	let io = IoHandler::new();
/// 	io.add_method("say_hello", SayHello);
/// 	let _server = Server::start(&"127.0.0.1:3030".parse().unwrap(), Arc::new(io), Some(AccessControlAllowOrigin::Null));
/// }
/// ```
pub struct Server {
	server: Option<hyper::server::Listening>,
	panic_handler: Arc<Mutex<Option<Box<Fn() -> () + Send>>>>
}

impl Server {
	pub fn start(addr: &SocketAddr, jsonrpc_handler: Arc<IoHandler>, cors_domain: Option<AccessControlAllowOrigin>) -> ServerResult {
		let panic_handler = Arc::new(Mutex::new(None));
		let panic_for_server = panic_handler.clone();
		let srv = try!(try!(hyper::Server::http(addr)).handle(move |_| {
			let handler = PanicHandler { handler: panic_for_server.clone() };
			ServerHandler::new(jsonrpc_handler.clone(), cors_domain.clone(), handler)
		}));
		Ok(Server {
			server: Some(srv),
			panic_handler: panic_handler,
		})
	}
	
	pub fn set_panic_handler<F>(&self, handler: F) 
		where F : Fn() -> () + Send + 'static {
		*self.panic_handler.lock().unwrap() = Some(Box::new(handler));
	}
}

impl Drop for Server {
	fn drop(&mut self) {
		self.server.take().unwrap().close()
	}
}
