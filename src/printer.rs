use crate::config::AppConfig;
use std::{
    error::Error,
    io::{self, Write},
    net::{IpAddr, Shutdown, SocketAddr, TcpStream},
    time::Duration,
};

pub fn send_to_printer(config: &AppConfig, message: &[u8]) -> Result<SocketAddr, Box<dyn Error>> {
    let ip = config
        .printer_ip
        .as_deref()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "printer_ip is missing"))?
        .parse::<IpAddr>()?;
    let port = config
        .printer_port
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "printer_port is missing"))?;
    let address = SocketAddr::new(ip, port);

    send_to_address(address, message)?;
    Ok(address)
}

fn send_to_address(address: SocketAddr, message: &[u8]) -> io::Result<()> {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(5))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    stream.write_all(message)?;
    stream.shutdown(Shutdown::Write)
}

#[cfg(test)]
mod tests {
    use super::send_to_address;
    use std::{
        io::Read,
        net::{Ipv4Addr, TcpListener},
        thread,
    };

    #[test]
    fn sends_all_message_bytes_over_tcp() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let receiver = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut received = Vec::new();
            stream.read_to_end(&mut received).unwrap();
            received
        });

        send_to_address(address, b"^Q1\n^E\n").unwrap();
        assert_eq!(receiver.join().unwrap(), b"^Q1\n^E\n");
    }
}
