use base64::{Engine as _, engine::general_purpose::STANDARD};
use ruprt::{config::AppConfig, db::latest_message, print_minimal_help};
use std::process;

struct Arguments {
    project_id: u32,
    line: Option<u32>,
}

fn parse_args(args: &[String]) -> Result<Arguments, String> {
    if args.is_empty() || args.len() > 3 {
        return Err("Usage: ruprt <project_ID> [-l line]".to_owned());
    }

    let project_id = args[0]
        .parse::<u32>()
        .map_err(|_| "project_ID must be an integer from 0 to 5000".to_owned())?;
    if project_id > 5000 {
        return Err("project_ID must be an integer from 0 to 5000".to_owned());
    }

    let line = match args.get(1).map(String::as_str) {
        None => None,
        Some("-l") => Some(
            args.get(2)
                .ok_or_else(|| "-l requires a line number".to_owned())?
                .parse::<u32>()
                .map_err(|_| "line must be a positive integer".to_owned())?,
        ),
        Some(_) => return Err("Usage: ruprt <project_ID> [-l line]".to_owned()),
    };

    Ok(Arguments { project_id, line })
}

fn ezpl_from_base64(encoded: &str) -> Result<String, Box<dyn std::error::Error>> {
    let decoded = STANDARD.decode(encoded.trim())?;
    let decoded = String::from_utf8(decoded)?;
    let mut commands = decoded
        .lines()
        .filter_map(|line| {
            let (prefix, command) = line.split_once("||")?;
            (prefix.len() == 3
                && prefix.starts_with('2')
                && prefix.bytes().all(|byte| byte.is_ascii_digit()))
            .then_some(command)
        })
        .collect::<Vec<_>>()
        .join("\n");

    if !commands.is_empty() {
        commands.push('\n');
    }
    Ok(commands)
}

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.is_empty() {
        print_minimal_help(
            "ruprt",
            env!("CARGO_PKG_VERSION"),
            "Loads and prepares a stored Godex EZPL print message.",
            "ruprt <project_ID> [-l line]",
            "config.toml: mysql_url = \"mysql://USER:PASSWORD@HOST:3306/\"",
        );
        return;
    }

    let parsed = match parse_args(&args) {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("{error}");
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
    let line = match parsed.line.or(config.default_line) {
        Some(line) => line,
        None => {
            eprintln!("Specify -l line or set default_line in config.toml");
            process::exit(2);
        }
    };

    let message = match latest_message(&config.mysql_url, parsed.project_id, line) {
        Ok(Some(message)) => message,
        Ok(None) => {
            eprintln!("No message found for project and line");
            process::exit(1);
        }
        Err(error) => {
            eprintln!("Database lookup failed: {error}");
            process::exit(1);
        }
    };

    match ezpl_from_base64(&message) {
        Ok(ezpl) => print!("{ezpl}"),
        Err(error) => {
            eprintln!("Unable to decode stored message: {error}");
            process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Arguments, ezpl_from_base64, parse_args};
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    fn parse(values: &[&str]) -> Result<Arguments, String> {
        let args = values
            .iter()
            .map(|value| (*value).to_owned())
            .collect::<Vec<_>>();
        parse_args(&args)
    }

    #[test]
    fn accepts_project_with_optional_line_override() {
        assert_eq!(parse(&["628"]).unwrap().project_id, 628);
        assert_eq!(parse(&["628", "-l", "3"]).unwrap().line, Some(3));
    }

    #[test]
    fn rejects_invalid_project_and_line_arguments() {
        assert!(parse(&["5001"]).is_err());
        assert!(parse(&["628", "-l"]).is_err());
        assert!(parse(&["628", "-l", "abc"]).is_err());
        assert!(parse(&["628", "extra"]).is_err());
    }

    #[test]
    fn decodes_and_keeps_only_ezpl_command_rows() {
        let message = STANDARD.encode("001||01\n201||^H15\n225||^C0\n");
        assert_eq!(ezpl_from_base64(&message).unwrap(), "^H15\n^C0\n");
    }
}
