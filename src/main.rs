use clap::Clap;
use itertools::Itertools;
use simple_error::{try_with, SimpleError};
use std::io::prelude::*;
use std::io::ErrorKind;
use std::net::{TcpListener, TcpStream};
use std::thread;

// Test with:
// curl -x <proxy_hostname>:<port> https:://<remote_host> --ssl-reqd

// The command line options.
#[derive(Clap, Debug)]
#[clap(name = "proctor")]
struct Opt {
    /// Debug mode
    #[clap(short, long)]
    debug: bool,

    /// Port to listen on
    #[clap(short, long)]
    port: i32,

    /// Remote servers that can be connected to on port 443
    #[clap(name = "HOSTNAME")]
    servers: Vec<String>,
}

fn main() -> Result<(), SimpleError> {
    let opt = Opt::parse();
    if opt.debug {
        println!("{:#?}", opt);
    }

    if opt.servers.len() == 0 {
        println!("You must provide at least one HOSTNAME as an allowed endpoint.");
        return Err(SimpleError::new("expected at least one server"));
    }

    let listen = try_with!(
        TcpListener::bind(format!("127.0.0.1:{}", opt.port)),
        "failed to bind"
    );

    // For now this accepts many connections but only proxies one at a time.
    for s in listen.incoming() {
        let mut stream = try_with!(s, "fail to listen");
        if opt.debug {
            println!("Incomming connection");
        }

        // default read/write timeouts of 5 seconds. This time out might be a little
        // low but I didn't want to deal with dangling connections.
        try_with!(
            stream.set_read_timeout(Some(std::time::Duration::new(5, 0))),
            "failed to set read timeout"
        );
        try_with!(
            stream.set_write_timeout(Some(std::time::Duration::new(5, 0))),
            "failed to set write timeout"
        );
        let (hostname, port) =
            try_with!(parse_http_connect(Bytes::new(&mut stream), opt.debug), "failed to begin");

        // make sure this is an allowed hostname
        guard(
            opt.servers.contains(&hostname),
            &format!("{} is not an allowed hostname", hostname),
        )?;

        // We should also make sure this is the SSL port.  This check
        // would be a good thing to add knobs and dials for in the CLI
        // options.
        guard(port == 443, &format!("Port must be 443"))?;

        // If we made it this far, we're finally ready to
        // tunnel traffic.
        proxy_connection(stream, &hostname, port, opt.debug)?;
    }
    Ok(())
}


const CONNECT: &'static str = "CONNECT";
const PROTOCOL: &'static str = "HTTP/1.1";
const RESPONSE_200: &'static str = "HTTP/1.1 200 OK\r\n\r\n";

/// This Bytes type is straight out of the rust standard library.  The
/// reason we need it at all is a little unfortunate. I wanted to do
/// the HTTP parsing manually using iterators, but std::io::Error
/// doesn't support Clone which made the whole iterator not support
/// Clone. And that made it incompatible with peeking and itertools
/// generally.
///
/// By making this wrapper, we have a chance to convert the error
/// types to something that is cloneable. In this case, I just grabbed
/// the SimpleError package because it was quick and easy. I'm just
/// going to be displaying all the errors to the user anyway. So in
/// this simple usage, I'm not losing much.
#[derive(Debug)]
pub struct Bytes<'a> {
    inner: &'a mut TcpStream,
}

impl<'a> Bytes<'a> {
    pub fn new(r: &'a mut TcpStream) -> Self {
        Bytes { inner: r }
    }
}

impl Iterator for Bytes<'_> {
    // Right here, this is the main difference from the definition of
    // this type that appears in the standard library.  the other
    // difference is that I removed the SizeHint stuff because it
    // didn't immediately compile and I didn't really need it.
    type Item = Result<u8, SimpleError>;

    fn next(&mut self) -> Option<Result<u8, SimpleError>> {
        let mut byte = 0;
        loop {
            return match self.inner.read(std::slice::from_mut(&mut byte)) {
                Ok(0) => None,
                Ok(..) => Some(Ok(byte)),
                Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => Some(Err(SimpleError::new(e.to_string()))),
            };
        }
    }
}

/// This just a convenience wrapper for enusuring certain
/// things hold during HTTP parsing.
fn guard(b: bool, msg: &str) -> Result<(), SimpleError> {
    if b {
        Ok(())
    } else {
        Err(SimpleError::new(msg.to_owned()))
    }
}

/// My idea for this was based on 2 things:
///
/// a) First, I knew I wanted to be precise about how much data I read
/// from the TcpStream, because anything that I overread I would need
/// to track in a buffer and send through the tunnel.
///
/// b) I thought it would be fun to use iterators in a fashion similar
/// to monadic parsing in functional languages.
///
/// However, I discovered that the default Bytes iterator doesn't work
/// well for this task for a few reasons:
///
/// a) It's not Clone-able due to a transitively missing Clone on the
/// error in the Result. That's why I have my own Bytes iterator.
///
/// b) Even though I've used the standard library iterators a fair bit
/// in the past I've never used them in a way where I take a bit and
/// then keep processing. Doing it this way I discovered that
/// `take_while` doesn't use peek() or anything like that and will
/// actually look at the next element but not put it back. To address
/// this, I used `peeking_take_while` from Itertools.
///
/// Knowing what I know now about the above two points, I think I
/// would approach this differently in the future. I think I would
/// probably make my own buffered reader that is a wrapper around the
/// TcpStream with a fixed size internal buffer. I wouldn't be
/// surprised if a nice type like this already exists.
///
/// Then for a parsing task like this, I would use non-blocking reads
/// into that buffer and extract things to strings splitting on spaces
/// and newlines as needed. After the HTTP parsing, I would then return
/// the unused portion of the buffer to feed into the tunneling step.
fn parse_http_connect(input: Bytes, debug: bool) -> Result<(String, i32), SimpleError> {
    let mut bytes = input.peekable();
    let len_connect = CONNECT.len();
    let connect_bytes = bytes
        .by_ref()
        .into_iter()
        .take(len_connect)
        .collect::<Result<Vec<u8>, SimpleError>>()?;
    // The first line of what we're trying to parse looks like this:
    // CONNECT <hostname>:<port> HTTP/1.1 \r\n

    let method = String::from_utf8_lossy(&connect_bytes);
    if debug {
        println!("method = {}", method);
    }
    guard(method == CONNECT, "Expected CONNECT method")?;

    // skip spaces
    bytes
        .peeking_take_while(|x| x.is_ok() && *(x.as_ref().unwrap()) as char == ' ')
        .collect::<Result<Vec<u8>, _>>()?;

    let hostname_bytes = bytes
        .peeking_take_while(|x| x.is_ok() && *(x.as_ref().unwrap()) as char != ':')
        .collect::<Result<Vec<u8>, _>>()?;
    let hostname = String::from_utf8_lossy(&hostname_bytes);
    if debug {
        println!("hostname = '{}'", hostname);
    }

    // skip the colon
    bytes
        .peeking_take_while(|x| x.is_ok() && *(x.as_ref().unwrap()) as char == ':')
        .collect::<Result<Vec<u8>, _>>()?;

    let port_bytes = bytes
        .peeking_take_while(|x| x.is_ok() && (*(x.as_ref().unwrap()) as char).is_digit(10))
        .collect::<Result<Vec<u8>, _>>()?;
    // Because we'll be converting the port number back to a string to
    // use it, we technically don't need to parse it. However, I do
    // like leaving this here as a bit of error detection.
    let port = try_with!(
        String::from_utf8_lossy(&port_bytes).parse::<i32>(),
        "failed to parse port number"
    );
    if debug {
        println!("port = {}", port);
    }

    // skip spaces
    bytes
        .peeking_take_while(|x| x.is_ok() && *(x.as_ref().unwrap()) as char == ' ')
        .collect::<Result<Vec<u8>, _>>()?;

    let protocol_bytes = bytes
        .peeking_take_while(|x| x.is_ok() && *(x.as_ref().unwrap()) as char != '\r')
        .collect::<Result<Vec<u8>, _>>()?;
    let protocol = String::from_utf8_lossy(&protocol_bytes);
    if debug {
        println!("protocol = '{}'", protocol);
    }
    guard(protocol == PROTOCOL, &format!("Expected {}", PROTOCOL))?;

    // CR NF (newline)
    // doubling up the collect() calls probably looks weird, but it's because
    // we can't chain the peeing_take_while(), and I thought it would be nice
    // to be precise about the order of the CR/LF characters
    bytes
        .peeking_take_while(|x| x.is_ok() && *(x.as_ref().unwrap()) as char == '\r')
        .collect::<Result<Vec<u8>, _>>()?;
    bytes
        .peeking_take_while(|x| x.is_ok() && *(x.as_ref().unwrap()) as char == '\n')
        .collect::<Result<Vec<u8>, _>>()?;

    // Now we need to parse header key/value pairs that are formatted like this:
    // <Key>: <Value>\r\n
    // The last line needs to be just a \r\n pair, then we know we're done.
    // all the bytes after that are meant for the remote end as they are part of
    // the TLS handshake.
    loop {
        // First we need to check for the termination condition, which
        // is an empty line ending in \r\n
        if bytes.peek() == Some(&Ok('\r' as u8)) {
            // we saw a carriage return so we are commited
            bytes.next();
            guard(bytes.peek() == Some(&Ok('\n' as u8)), "Expected newline")?;
            if debug {
                println!("End of connect request");
            }
            break;
        }

        // Okay, we didn't have an empty line, so we must have a header to parse
        let key_bytes = bytes
            .peeking_take_while(|x| x.is_ok() && *(x.as_ref().unwrap()) as char != ':')
            .collect::<Result<Vec<u8>, _>>()?;
        let key = String::from_utf8_lossy(&key_bytes);
        if debug {
            print!("{}: ", key);
        }
        // skip the colon
        bytes
            .peeking_take_while(|x| x.is_ok() && *(x.as_ref().unwrap()) as char == ':')
            .collect::<Result<Vec<u8>, _>>()?;

        // skip spaces
        bytes
            .peeking_take_while(|x| x.is_ok() && *(x.as_ref().unwrap()) as char == ' ')
            .collect::<Result<Vec<u8>, _>>()?;

        let value_bytes = bytes
            .peeking_take_while(|x| x.is_ok() && *(x.as_ref().unwrap()) as char != '\r')
            .collect::<Result<Vec<u8>, _>>()?;
        let value = String::from_utf8_lossy(&value_bytes);
        if debug {
            println!("{}", value);
        }

        // CR NF (newline)
        bytes
            .peeking_take_while(|x| x.is_ok() && *(x.as_ref().unwrap()) as char == '\r')
            .collect::<Result<Vec<u8>, _>>()?;
        bytes
            .peeking_take_while(|x| x.is_ok() && *(x.as_ref().unwrap()) as char == '\n')
            .collect::<Result<Vec<u8>, _>>()?;
    }

    // Finally we have a hostname and port
    Ok((hostname.to_string(), port))
}

/// This is whereh all the tunneling happens. The invariant here is
/// that the client stream is positioned so that the next read from
/// the client will be traffic intended for the remote server. For
/// example, a TLS handshake.
///
/// The general strategy is to clone the TcpStreams and then fork
/// to threads:
///
/// A thread to handle reading from the client and sending to the server.
///
/// A second thread to read from the server and send to the client.
///
/// I've named these downlink and uplink respectively, but my naming
/// is actually a little arbitrary since traffic is moving on both
/// sides in each thread. However, I think about it in terms of the
/// sending party.
fn proxy_connection(
    mut client_recv: TcpStream,
    hostname: &str,
    port: i32,
    debug: bool,
) -> Result<(), SimpleError> {
    use std::thread::JoinHandle;

    // To make the threading work out, it's useful to clone the
    // TcpStreams.  according to the documentation, the clones will
    // share the underlying TcpStream configuration such as timeout
    // and position in the stream.
    let mut client_send = try_with!(client_recv.try_clone(), "failed to clone client stream");

    // We should also establish a connection with the server at some point.
    let connection_string = format!("{}:{}", hostname, port);
    if debug {
        println!("connecting to {}", connection_string);
    }
    let mut server_recv = try_with!(
        TcpStream::connect(connection_string),
        "failed to connect to remote host"
    );

    // Just like the client case, we set some timeouts to prevent dangling connections
    try_with!(
        server_recv.set_read_timeout(Some(std::time::Duration::new(5, 0))),
        "failed to set read timeout"
    );
    try_with!(
        server_recv.set_write_timeout(Some(std::time::Duration::new(5, 0))),
        "failed to set write timeout"
    );

    let mut server_send = try_with!(server_recv.try_clone(), "failed to clone server stream");

    if debug {
        println!("Sending OK to client");
    }

    // If we made it this far we have both connections, so we should let the client know
    // that the tunnel is established and proxying can commence.
    try_with!(
        client_send.write_all(RESPONSE_200.as_bytes()),
        "failed to send OK to client"
    );

    // Finally, a thread for reading from the client and sending to
    // the server. You could think of this as uploading, however, I
    // prefer to think of it more like a firewall rule where it's from
    // the perspective of the interface where the traffic originated.
    let downlink: JoinHandle<()> = thread::spawn(move || loop {
        let mut buffer = [0; 1024];
        if debug {
            println!("downlink about to read from client");
        }
        let count = match client_recv.read(&mut buffer) {
            Ok(c) => c,
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::InvalidInput =>
            {
                if debug {
                    println!("downlink error but retrying");
                }
                continue;
            }
            Err(_) => return,
        };
        if debug && count == 0 {
            println!("downlink: read 0 and returning");
        }
        // This is not a great way to detect that the client is done,
        // however, I couldn't (quickly) determine from the TcpStream
        // docs if there is a better way to detect this. In practice
        // it seems to be fine. The same pattern occurs in the thread
        // below.
        if count == 0 {
            return;
        }
        let server_ok = match server_send.write_all(&buffer[0..count]) {
            Ok(_) => true,
            Err(_) => false,
        };
        if !server_ok {
            return;
        }
    });

    // Now a thread for the other direction.
    let uplink: JoinHandle<()> = thread::spawn(move || loop {
        let mut buffer = [0; 1024];
        if debug {
            println!("uplink about to read from server");
        }
        let count = match server_recv.read(&mut buffer) {
            Ok(c) => c,
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::InvalidInput =>
            {
                if debug {
                    println!("uplink error but retrying");
                }
                continue;
            }
            Err(e) => {
                println!("e = {}", e.to_string());
                return;
            }
        };
        if debug && count == 0 {
            println!("uplink: read 0 and returning");
        }
        if count == 0 {
            return;
        }
        let client_ok = match client_send.write_all(&buffer[0..count]) {
            Ok(_) => true,
            Err(_) => false,
        };
        if !client_ok {
            return;
        }
    });

    // If we wanted to support simultaneous tunnels we would need to
    // move these joins out to the loop in main.
    uplink
        .join()
        .map_err(|_| SimpleError::new("Failed to join on uplink thread"))?;
    if debug {
        println!("uplink finished");
    }
    downlink
        .join()
        .map_err(|_| SimpleError::new("Failed to join on downlink thread"))?;
    if debug {
        println!("downlink finished");
    }
    Ok(())
}
