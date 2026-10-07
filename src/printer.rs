use crate::config::AppConfig;
use std::{
    error::Error,
    fs::OpenOptions,
    io::{self, Write},
    net::{IpAddr, Shutdown, SocketAddr, TcpStream},
    path::Path,
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

    send_to_address_with_log(address, message, Path::new("print.log"))?;
    Ok(address)
}

fn write_print_log(path: &Path, message: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)?;
    file.write_all(message)
}

fn send_to_address_with_log(
    address: SocketAddr,
    message: &[u8],
    log_path: &Path,
) -> io::Result<()> {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(5))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    write_print_log(log_path, message)?;
    stream.write_all(message)?;
    stream.shutdown(Shutdown::Write)
}

#[cfg(test)]
mod tests {
    use super::{send_to_address_with_log, write_print_log};
    use std::{
        io::Read,
        net::{Ipv4Addr, TcpListener},
        thread,
    };

    #[test]
    fn sends_all_message_bytes_over_tcp() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let log_path =
            std::env::temp_dir().join(format!("ruprt-send-log-{}.tmp", std::process::id()));
        let receiver = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut received = Vec::new();
            stream.read_to_end(&mut received).unwrap();
            received
        });

        send_to_address_with_log(address, b"^Q1\n^E\n", &log_path).unwrap();
        assert_eq!(receiver.join().unwrap(), b"^Q1\n^E\n");
        assert_eq!(std::fs::read(&log_path).unwrap(), b"^Q1\n^E\n");
        std::fs::remove_file(log_path).unwrap();
    }

    #[test]
    fn overwrites_print_log_with_exact_message_bytes() {
        let path = std::env::temp_dir().join(format!("ruprt-print-log-{}.tmp", std::process::id()));
        write_print_log(&path, b"first print\n").unwrap();
        write_print_log(&path, b"second").unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        std::fs::remove_file(path).unwrap();
    }
}
