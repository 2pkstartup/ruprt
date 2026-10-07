use ruprt::{config::AppConfig, print_minimal_help, printer::send_to_printer};
use std::{
    error::Error,
    io::{self, IsTerminal, Read},
    process,
};

/// Reads an exact payload from one argument or stdin; stdin is consumed to EOF.
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

/// Sends the supplied raw bytes to the configured printer without altering them.
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
    match send_to_printer(&config, &message) {
        Ok(address) => eprintln!("Sent {} bytes to {address}", message.len()),
        Err(error) => {
            eprintln!("Failed to send message to printer: {error}");
            process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::read_message;
    use std::io::Cursor;

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
}
