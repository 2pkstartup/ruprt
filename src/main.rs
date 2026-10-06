use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::{Local, NaiveDate};
use ruprt::{
    config::AppConfig,
    db::{calculate_project_lot, latest_message},
    print_minimal_help,
};
use std::process;

struct Arguments {
    project_id: u32,
    line: Option<u32>,
    date: Option<NaiveDate>,
    x_offset: i64,
    y_offset: i64,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct CoordinateMinima {
    x: Option<i32>,
    y: Option<i32>,
}

#[derive(Debug, PartialEq, Eq)]
struct ProcessedMessage {
    ezpl: String,
    minima: CoordinateMinima,
}

fn message_template_line(requested_line: u32) -> u32 {
    if (2..=4).contains(&requested_line) {
        2
    } else {
        requested_line
    }
}

fn parse_args(args: &[String]) -> Result<Arguments, String> {
    if args.is_empty() {
        return Err("Usage: ruprt <project_ID> [-l line] [-d YYMMDD]".to_owned());
    }

    let project_id = args[0]
        .parse::<u32>()
        .map_err(|_| "project_ID must be an integer from 0 to 5000".to_owned())?;
    if project_id > 5000 {
        return Err("project_ID must be an integer from 0 to 5000".to_owned());
    }

    let mut line = None;
    let mut date = None;
    let mut x_offset = None;
    let mut y_offset = None;
    let mut index = 1;
    while index < args.len() {
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("{} requires a value", args[index]))?;
        match args[index].as_str() {
            "-l" if line.is_none() => {
                line = Some(
                    value
                        .parse::<u32>()
                        .map_err(|_| "line must be a positive integer".to_owned())?,
                );
            }
            "-d" if date.is_none() => date = Some(parse_yymmdd(value)?),
            "-x" if x_offset.is_none() => {
                x_offset = Some(
                    value
                        .parse::<i64>()
                        .map_err(|_| "x offset must be a signed integer".to_owned())?,
                );
            }
            "-y" if y_offset.is_none() => {
                y_offset = Some(
                    value
                        .parse::<i64>()
                        .map_err(|_| "y offset must be a signed integer".to_owned())?,
                );
            }
            _ => {
                return Err(
                    "Usage: ruprt <project_ID> [-l line] [-d YYMMDD] [-x offset] [-y offset]"
                        .to_owned(),
                );
            }
        }
        index += 2;
    }

    Ok(Arguments {
        project_id,
        line,
        date,
        x_offset: x_offset.unwrap_or(0),
        y_offset: y_offset.unwrap_or(0),
    })
}

fn parse_yymmdd(value: &str) -> Result<NaiveDate, String> {
    if value.len() != 6 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("date must use YYMMDD format".to_owned());
    }

    let year = 2000 + value[..2].parse::<i32>().map_err(|_| "invalid date")?;
    let month = value[2..4].parse::<u32>().map_err(|_| "invalid date")?;
    let day = value[4..6].parse::<u32>().map_err(|_| "invalid date")?;
    NaiveDate::from_ymd_opt(year, month, day).ok_or_else(|| "invalid date".to_owned())
}

fn coordinates_for_row(prefix: &str, command: &str) -> Option<(i32, i32)> {
    match prefix {
        "231" => {
            let mut fields = command.split(',');
            fields.next()?;
            Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
        }
        "232" => {
            let (command_and_x, remainder) = command.split_once(',')?;
            let x_start = command_and_x.find(|character: char| character.is_ascii_digit())?;
            let x = command_and_x[x_start..].parse().ok()?;
            let y = remainder.split(',').next()?.parse().ok()?;
            Some((x, y))
        }
        _ => None,
    }
}

fn update_minima(minima: &mut CoordinateMinima, x: i32, y: i32) {
    minima.x = Some(minima.x.map_or(x, |current| current.min(x)));
    minima.y = Some(minima.y.map_or(y, |current| current.min(y)));
}

fn shift_coordinate(value: &str, offset: i64) -> Result<String, String> {
    let coordinate = value
        .parse::<i64>()
        .map_err(|_| "coordinate must be a non-negative integer".to_owned())?;
    let shifted = coordinate.saturating_add(offset).max(0);
    Ok(format!("{shifted:0width$}", width = value.len()))
}

fn shift_row_coordinates(
    prefix: &str,
    command: &str,
    x_offset: i64,
    y_offset: i64,
) -> Result<String, String> {
    if x_offset == 0 && y_offset == 0 {
        return Ok(command.to_owned());
    }

    match prefix {
        "231" => {
            let mut fields = command.split(',').map(str::to_owned).collect::<Vec<_>>();
            if fields.len() < 3 {
                return Err("EZPL 231 row has no X/Y coordinates".to_owned());
            }
            fields[1] = shift_coordinate(&fields[1], x_offset)?;
            fields[2] = shift_coordinate(&fields[2], y_offset)?;
            Ok(fields.join(","))
        }
        "232" => {
            let (command_and_x, remainder) = command
                .split_once(',')
                .ok_or_else(|| "EZPL 232 row has no Y coordinate".to_owned())?;
            let x_start = command_and_x
                .find(|character: char| character.is_ascii_digit())
                .ok_or_else(|| "EZPL 232 row has no X coordinate".to_owned())?;
            let command_name = &command_and_x[..x_start];
            let x = shift_coordinate(&command_and_x[x_start..], x_offset)?;
            let (y_value, remaining_fields) = remainder
                .split_once(',')
                .map_or((remainder, None), |(y, rest)| (y, Some(rest)));
            let y = shift_coordinate(y_value, y_offset)?;

            Ok(match remaining_fields {
                Some(rest) => format!("{command_name}{x},{y},{rest}"),
                None => format!("{command_name}{x},{y}"),
            })
        }
        _ => Ok(command.to_owned()),
    }
}

fn ezpl_from_base64(
    encoded: &str,
    calculated_lot: &str,
    x_offset: i64,
    y_offset: i64,
) -> Result<ProcessedMessage, Box<dyn std::error::Error>> {
    let decoded = STANDARD.decode(encoded.trim())?;
    let decoded = String::from_utf8(decoded)?;
    let mut commands = Vec::new();
    let mut minima = CoordinateMinima::default();
    for row in decoded.lines() {
        let Some((prefix, command)) = row.split_once("||") else {
            continue;
        };
        if let Some((x, y)) = coordinates_for_row(prefix, command) {
            update_minima(&mut minima, x, y);
        }
        if prefix.len() != 3
            || !prefix.starts_with('2')
            || !prefix.bytes().all(|byte| byte.is_ascii_digit())
        {
            continue;
        }

        if prefix == "210" {
            let Some((command, _old_lot)) = command.rsplit_once(',') else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "EZPL 210 row has no lot field",
                )
                .into());
            };
            commands.push(format!("{command},{calculated_lot}"));
        } else {
            commands.push(shift_row_coordinates(prefix, command, x_offset, y_offset)?);
        }
    }

    let mut commands = commands.join("\n");
    if !commands.is_empty() {
        commands.push('\n');
    }
    Ok(ProcessedMessage {
        ezpl: commands,
        minima,
    })
}

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.is_empty() {
        print_minimal_help(
            "ruprt",
            env!("CARGO_PKG_VERSION"),
            "Loads and prepares a stored Godex EZPL print message.",
            "ruprt <project_ID> [-l line] [-d YYMMDD] [-x offset] [-y offset]",
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

    let date = parsed.date.unwrap_or_else(|| Local::now().date_naive());
    let calculated_lot = match calculate_project_lot(&config.mysql_url, parsed.project_id, date) {
        Ok(Some(lot)) => lot,
        Ok(None) => {
            eprintln!("Project has no usable DateCode or LOT result");
            process::exit(1);
        }
        Err(error) => {
            eprintln!("Lot calculation failed: {error}");
            process::exit(1);
        }
    };

    // Keep `line` unchanged for later EZPL edits; only template lookup is shared.
    let message_line = message_template_line(line);
    let message = match latest_message(&config.mysql_url, parsed.project_id, message_line) {
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

    match ezpl_from_base64(&message, &calculated_lot, parsed.x_offset, parsed.y_offset) {
        Ok(processed) => {
            let _coordinate_minima = (processed.minima.x, processed.minima.y);
            print!("{}", processed.ezpl);
        }
        Err(error) => {
            eprintln!("Unable to decode stored message: {error}");
            process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Arguments, ezpl_from_base64, message_template_line, parse_args, parse_yymmdd};
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use chrono::NaiveDate;

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
    fn accepts_optional_date_in_yymmdd_format() {
        assert_eq!(
            parse(&["628", "-d", "261001"]).unwrap().date,
            Some(NaiveDate::from_ymd_opt(2026, 10, 1).unwrap())
        );
        assert_eq!(parse_yymmdd("261332"), Err("invalid date".to_owned()));
    }

    #[test]
    fn maps_lines_two_through_four_to_the_shared_template_line() {
        assert_eq!(message_template_line(2), 2);
        assert_eq!(message_template_line(3), 2);
        assert_eq!(message_template_line(4), 2);
        assert_eq!(message_template_line(1), 1);
        assert_eq!(message_template_line(5), 5);
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
        assert_eq!(
            ezpl_from_base64(&message, "LOT42", 0, 0).unwrap().ezpl,
            "^H15\n^C0\n"
        );
    }

    #[test]
    fn replaces_the_text_after_the_last_comma_in_the_210_row() {
        let message = STANDARD.encode("210||AD,0035,0245,1,1,0,0,OLD-LOT\n201||^E\n");
        assert_eq!(
            ezpl_from_base64(&message, "NEW-LOT", 0, 0).unwrap().ezpl,
            "AD,0035,0245,1,1,0,0,NEW-LOT\n^E\n"
        );
    }

    #[test]
    fn stores_the_smallest_coordinates_from_231_and_232_rows() {
        let message =
            STANDARD.encode("231||AD,0035,0245,1,1\n232||W0300,0045,3,2\n232||XRB0020,0030,6,2\n");
        let processed = ezpl_from_base64(&message, "LOT42", 0, 0).unwrap();

        assert_eq!(processed.minima.x, Some(20));
        assert_eq!(processed.minima.y, Some(30));
    }

    #[test]
    fn applies_signed_offsets_and_clamps_coordinates_to_zero() {
        let message =
            STANDARD.encode("231||AD,0035,0245,1,1\n232||W0300,0045,3,2\n232||XRB0020,0030,6,2\n");
        let processed = ezpl_from_base64(&message, "LOT42", -50, 10).unwrap();

        assert_eq!(
            processed.ezpl,
            "AD,0000,0255,1,1\nW0250,0055,3,2\nXRB0000,0040,6,2\n"
        );
    }

    #[test]
    fn clamps_negative_coordinate_results_to_zero() {
        let message = STANDARD.encode("231||AD,0005,0003,1,1\n");
        let processed = ezpl_from_base64(&message, "LOT42", -10, -10).unwrap();
        assert_eq!(processed.ezpl, "AD,0000,0000,1,1\n");
    }
}
