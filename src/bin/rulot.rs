use chrono::NaiveDate;
use ruprt::{config::AppConfig, db::lot_to_date, print_long_help};
use std::process;

struct Arguments {
    project_id: u32,
    lot: String,
}

fn parse_args(args: &[String]) -> Result<Arguments, String> {
    let usage = "Usage: rulot <project_ID> <lot> | rulot <project_ID> -d <lot>";
    if args.len() != 2 && args.len() != 3 {
        return Err(usage.to_owned());
    }

    let project_id = args[0]
        .parse::<u32>()
        .map_err(|_| "project_ID must be an integer from 0 to 5000".to_owned())?;
    if project_id > 5000 {
        return Err("project_ID must be an integer from 0 to 5000".to_owned());
    }

    let lot = match args {
        [_, lot] if lot != "-d" => lot.as_str(),
        [_, flag, lot] if flag == "-d" => lot.as_str(),
        _ => return Err(usage.to_owned()),
    };
    if lot.trim().is_empty() {
        return Err("lot must not be empty".to_owned());
    }

    Ok(Arguments {
        project_id,
        lot: lot.to_owned(),
    })
}

fn format_yymmdd(date: NaiveDate) -> String {
    date.format("%y%m%d").to_string()
}

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() == 1 && matches!(args[0].as_str(), "--help" | "-h") {
        print_long_help(
            "rulot",
            env!("CARGO_PKG_VERSION"),
            "Converts a project lot code to its production date.",
            "rulot <project_ID> <lot> | rulot <project_ID> -d <lot>",
            &[
                ("project_ID", "Project number, integer from 0 to 5000."),
                ("lot", "Lot code understood by specs.LOT_TO_DATE."),
                (
                    "-d LOT",
                    "Optional explicit flag form for the lot argument.",
                ),
                ("--help, -h", "Show this help."),
            ],
            &["rulot 945 XK03", "rulot 945 -d XK03"],
            "config.toml: mysql_url = \"mysql://USER:PASSWORD@HOST:3306/\"",
        );
        return;
    }
    if args.is_empty() {
        print_long_help(
            "rulot",
            env!("CARGO_PKG_VERSION"),
            "Converts a project lot code to its production date.",
            "rulot <project_ID> <lot> | rulot <project_ID> -d <lot>",
            &[
                ("project_ID", "Project number, integer from 0 to 5000."),
                ("lot", "Lot code understood by specs.LOT_TO_DATE."),
                (
                    "-d LOT",
                    "Optional explicit flag form for the lot argument.",
                ),
                ("--help, -h", "Show this help."),
            ],
            &["rulot 945 XK03", "rulot 945 -d XK03"],
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

    let date = match lot_to_date(&config.mysql_url, parsed.project_id, &parsed.lot) {
        Ok(Some(date)) => date,
        Ok(None) => {
            eprintln!(
                "No date mapping for project {} and lot {}",
                parsed.project_id, parsed.lot
            );
            process::exit(1);
        }
        Err(error) => {
            eprintln!("Lot-to-date lookup failed: {error}");
            process::exit(1);
        }
    };

    println!("{}", format_yymmdd(date));
}

#[cfg(test)]
mod tests {
    use super::{Arguments, format_yymmdd, parse_args};
    use chrono::NaiveDate;

    fn parse(values: &[&str]) -> Result<Arguments, String> {
        let args = values
            .iter()
            .map(|value| (*value).to_owned())
            .collect::<Vec<_>>();
        parse_args(&args)
    }

    #[test]
    fn accepts_positional_and_flagged_lot_forms() {
        assert_eq!(parse(&["945", "XK03"]).unwrap().lot, "XK03");
        assert_eq!(parse(&["945", "-d", "XK03"]).unwrap().lot, "XK03");
    }

    #[test]
    fn validates_project_and_lot_arguments() {
        assert!(parse(&["5001", "XK03"]).is_err());
        assert!(parse(&["945", "-d"]).is_err());
        assert!(parse(&["945", ""]).is_err());
        assert!(parse(&["945", "XK03", "extra"]).is_err());
    }

    #[test]
    fn formats_dates_as_yymmdd() {
        assert_eq!(
            format_yymmdd(NaiveDate::from_ymd_opt(2026, 10, 15).unwrap()),
            "261015"
        );
    }
}
