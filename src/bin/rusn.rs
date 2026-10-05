use ruprt::{
    config::AppConfig,
    db::{find_max_serial_number, find_serial_config, lot_to_date},
    print_minimal_help,
};
use std::process;

#[derive(Debug, PartialEq, Eq)]
struct Arguments {
    project_id: u32,
    lot: String,
}

/// Accepts exactly a project ID and its lot code.
fn parse_args(args: &[String]) -> Result<Arguments, String> {
    if args.len() != 2 {
        return Err("Usage: rusn <project_ID> <lot>".to_owned());
    }

    let project_id = args[0]
        .parse::<u32>()
        .map_err(|_| "project_ID must be an integer from 0 to 5000".to_owned())?;
    if project_id > 5000 {
        return Err("project_ID must be an integer from 0 to 5000".to_owned());
    }

    let lot = args[1].trim();
    if lot.is_empty() {
        return Err("lot must not be empty".to_owned());
    }

    Ok(Arguments {
        project_id,
        lot: lot.to_owned(),
    })
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        print_minimal_help(
            "rusn",
            env!("CARGO_PKG_VERSION"),
            "Finds the highest serial number for a project and lot.",
            "rusn <project_ID> <lot>",
            "config.toml: mysql_url = \"mysql://USER:PASSWORD@HOST:3306/\"",
        );
        return;
    }

    let parsed = match parse_args(&args) {
        Ok(parsed) => parsed,
        Err(_) => process::exit(2),
    };

    let config = match AppConfig::load() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("Failed to load config.toml: {error}");
            process::exit(1);
        }
    };

    let is_serial = match find_serial_config(&config.mysql_url, parsed.project_id) {
        Ok(result) => result.is_some(),
        Err(error) => {
            eprintln!("Database lookup failed: {error}");
            process::exit(1);
        }
    };
    if !is_serial {
        println!("-1");
        return;
    }

    let date_of_print = match lot_to_date(&config.mysql_url, parsed.project_id, &parsed.lot) {
        Ok(Some(date)) => date,
        Ok(None) => {
            println!("0");
            return;
        }
        Err(error) => {
            eprintln!("Lot date lookup failed: {error}");
            process::exit(1);
        }
    };

    // Database and stored-procedure errors go to stderr; successful stdout stays numeric-only.
    let lookup = match find_max_serial_number(&config.mysql_url, parsed.project_id, date_of_print) {
        Ok(result) => result,
        Err(error) => {
            eprintln!("Database lookup failed: {error}");
            process::exit(1);
        }
    };

    // A non-serialized project uses -1; serialized projects return the procedure's maximum.
    if lookup.is_serial {
        println!("{}", lookup.max_serial);
    } else {
        println!("-1");
    }
}

#[cfg(test)]
mod tests {
    use super::{Arguments, parse_args};

    fn parse(values: &[&str]) -> Result<Arguments, String> {
        let args: Vec<String> = values.iter().map(|value| (*value).to_owned()).collect();
        parse_args(&args)
    }

    #[test]
    fn accepts_project_id_bounds_and_lot() {
        assert_eq!(
            parse(&["0", "XE15"]),
            Ok(Arguments {
                project_id: 0,
                lot: "XE15".to_owned(),
            })
        );
        assert_eq!(
            parse(&["5000", "XE15"]),
            Ok(Arguments {
                project_id: 5000,
                lot: "XE15".to_owned(),
            })
        );
    }

    #[test]
    fn accepts_four_character_lot_code() {
        assert_eq!(parse(&["628", "XE15"]).unwrap().lot, "XE15");
    }

    #[test]
    fn rejects_invalid_project_ids_empty_lots_and_extra_arguments() {
        assert!(parse(&["-1"]).is_err());
        assert!(parse(&["5001"]).is_err());
        assert!(parse(&["0"]).is_err());
        assert!(parse(&["945", ""]).is_err());
        assert!(parse(&["945", "XE15", "extra"]).is_err());
        assert!(parse(&[]).is_err());
    }
}
