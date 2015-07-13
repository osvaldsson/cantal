use std::io;
use std::net::SocketAddr;

use mio::EventLoop;
use mio::tcp::TcpStream;

use super::super::server::Handler;


pub struct WebSocket {
    sock: TcpStream,
    input: Vec<u8>,
    output: Vec<u8>,
}


impl WebSocket {
    pub fn connect(addr: SocketAddr) -> Result<WebSocket, io::Error>
    {
        Ok(WebSocket {
            sock: try!(TcpStream::connect(&addr)),
            input: Vec::new(),
            output: Vec::new(),
        })
    }
}
