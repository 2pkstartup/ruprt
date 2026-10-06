use ruprt::{config::AppConfig, print_minimal_help};
use std::{
    error::Error,
    io::{self, IsTerminal, Read, Write},
    net::{IpAddr, Shutdown, SocketAddr, TcpStream},
    process,
    time::Duration,
};

fn read_message(args: &[String], mut stdin: impl Read) -> Result<Vec<u8>, Box<dyn Error>> {
    let message = match args {
        [message] => message.as_bytes().to_vec(),
        [] => {
            let mut message = Vec::new();
            stdin.read_to_end(&mut message)?;
            message
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "pass the message as one argument or through stdin",
            )
            .into());
        }
    };

    if message.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "message is empty").into());
    }

    Ok(message)
}

fn send_message(address: SocketAddr, message: &[u8]) -> io::Result<()> {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(5))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    stream.write_all(message)?;
    stream.shutdown(Shutdown::Write)
}

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.is_empty() && io::stdin().is_terminal() {
        print_minimal_help(
            "rusend",
            env!("CARGO_PKG_VERSION"),
            "Sends an EZPL message to the configured printer over TCP.",
            "rusend <message> | command | rusend < message.ezpl",
            "config.toml: printer_ip = \"192.0.2.10\", printer_port = 9100",
        );
        return;
    }

    let message = match read_message(&args, io::stdin().lock()) {
        Ok(message) => message,
        Err(error) => {
            eprintln!("Unable to read message: {error}");
            process::exit(2);
        }
    };

    let config = match AppConfig::load() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("Failed to load config.toml: {error}");
            process::exit(1);
        }
    };
    let ip = match config.printer_ip.as_deref().map(str::parse::<IpAddr>) {
        Some(Ok(ip)) => ip,
        Some(Err(error)) => {
            eprintln!("Invalid printer_ip: {error}");
            process::exit(2);
        }
        None => {
            eprintln!("printer_ip is missing from config.toml");
            process::exit(2);
        }
    };
    let port = match config.printer_port {
        Some(port) => port,
        None => {
            eprintln!("printer_port is missing from config.toml");
            process::exit(2);
        }
    };

    let address = SocketAddr::new(ip, port);
    if let Err(error) = send_message(address, &message) {
        eprintln!("Failed to send message to printer: {error}");
        process::exit(1);
    }
    eprintln!("Sent {} bytes to {address}", message.len());
}

#[cfg(test)]
mod tests {
    use super::{read_message, send_message};
    use std::{
        io::Cursor,
        net::{Ipv4Addr, TcpListener},
        thread,
    };

    #[test]
    fn reads_message_from_argument_or_stdin_without_changing_bytes() {
        assert_eq!(
            read_message(&["^Q1".to_owned()], Cursor::new(b"unused".to_vec())).unwrap(),
            b"^Q1"
        );
        assert_eq!(
            read_message(&[], Cursor::new(b"^Q1\n^E\n".to_vec())).unwrap(),
            b"^Q1\n^E\n"
        );
    }

    #[test]
    fn rejects_empty_message_and_multiple_arguments() {
        assert!(read_message(&[], Cursor::new(Vec::new())).is_err());
        assert!(read_message(&["a".to_owned(), "b".to_owned()], Cursor::new(Vec::new())).is_err());
    }

    #[test]
    fn sends_all_message_bytes_over_tcp() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let receiver = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut received = Vec::new();
            std::io::Read::read_to_end(&mut stream, &mut received).unwrap();
            received
        });

        send_message(address, b"^Q1\n^E\n").unwrap();
        assert_eq!(receiver.join().unwrap(), b"^Q1\n^E\n");
    }
}
