use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::{Local, NaiveDate};
use ruprt::{
    config::AppConfig,
    db::{calculate_project_lot, find_max_serial_number, generate_serial_code, latest_message},
    print_minimal_help,
    printer::send_to_printer,
};
use std::{
    fs::OpenOptions,
    io::{self, Write},
    path::Path,
    process,
};

struct Arguments {
    project_id: u32,
    line: Option<u32>,
    date: Option<NaiveDate>,
    x_offset: i64,
    y_offset: i64,
    test_mode: bool,
    print_overrides: PrintOverrides,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct PrintOverrides {
    copies: Option<u32>,
    temperature: Option<u32>,
    speed: Option<u32>,
    stop_possition: Option<i32>,
    layout_horizontal: Option<u32>,
    layout_vertical: Option<i32>,
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

fn usable_serial_start(is_serial: bool, serial_number: u64) -> Option<u64> {
    (is_serial && serial_number > 0).then_some(serial_number)
}

fn parse_args(args: &[String]) -> Result<Arguments, String> {
    if args.is_empty() {
        return Err("Usage: ruprt <project_ID> [-t [test]] [-l line] [-d YYMMDD] [-p count] [-h temp] [-s speed] [-e value] [-r value] [-q value] [-x offset] [-y offset]".to_owned());
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
    let mut copies = None;
    let mut temperature = None;
    let mut speed = None;
    let mut stop_possition = None;
    let mut layout_horizontal = None;
    let mut layout_vertical = None;
    let mut test_mode = false;
    let mut index = 1;
    while index < args.len() {
        if args[index] == "-t" && !test_mode {
            test_mode = true;
            if args.get(index + 1).is_some_and(|value| value == "test") {
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }

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
            "-p" if copies.is_none() => {
                copies = Some(parse_bounded_value(value, "print count", 1, 20_000)?);
            }
            "-h" if temperature.is_none() => {
                temperature = Some(parse_bounded_value(value, "temperature", 0, 20)?);
            }
            "-s" if speed.is_none() => {
                speed = Some(parse_bounded_value(value, "print speed", 2, 6)?);
            }
            "-e" if stop_possition.is_none() => {
                stop_possition = Some(parse_bounded_signed_value(
                    value,
                    "stop_possition",
                    -40,
                    40,
                )?);
            }
            "-r" if layout_horizontal.is_none() => {
                layout_horizontal = Some(parse_bounded_value(value, "horizontal layout", 0, 100)?);
            }
            "-q" if layout_vertical.is_none() => {
                layout_vertical = Some(parse_bounded_signed_value(
                    value,
                    "vertical layout",
                    -100,
                    100,
                )?);
            }
            _ => {
                return Err("Usage: ruprt <project_ID> [-t [test]] [-l line] [-d YYMMDD] [-p count] [-h temp] [-s speed] [-e value] [-r value] [-q value] [-x offset] [-y offset]".to_owned());
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
        test_mode,
        print_overrides: PrintOverrides {
            copies,
            temperature,
            speed,
            stop_possition,
            layout_horizontal,
            layout_vertical,
        },
    })
}

fn parse_bounded_value(value: &str, name: &str, minimum: u32, maximum: u32) -> Result<u32, String> {
    let parsed = value
        .parse::<u32>()
        .map_err(|_| format!("{name} must be an integer from {minimum} to {maximum}"))?;
    if !(minimum..=maximum).contains(&parsed) {
        return Err(format!(
            "{name} must be an integer from {minimum} to {maximum}"
        ));
    }
    Ok(parsed)
}

fn parse_bounded_signed_value(
    value: &str,
    name: &str,
    minimum: i32,
    maximum: i32,
) -> Result<i32, String> {
    let parsed = value
        .parse::<i32>()
        .map_err(|_| format!("{name} must be an integer from {minimum} to {maximum}"))?;
    if !(minimum..=maximum).contains(&parsed) {
        return Err(format!(
            "{name} must be an integer from {minimum} to {maximum}"
        ));
    }
    Ok(parsed)
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

fn message_has_prefix(
    encoded: &str,
    target_prefix: &str,
) -> Result<bool, Box<dyn std::error::Error>> {
    let decoded = STANDARD.decode(encoded.trim())?;
    let decoded = String::from_utf8(decoded)?;
    Ok(decoded.lines().any(|row| {
        row.split_once("||")
            .is_some_and(|(prefix, _)| prefix == target_prefix)
    }))
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

fn replace_number_after_marker(
    command: &str,
    marker: &str,
    value: &str,
) -> Result<Option<String>, String> {
    let Some(marker_start) = command.find(marker) else {
        return Ok(None);
    };
    let value_start = marker_start + marker.len();
    let digits_start = if command[value_start..].starts_with(['-', '+']) {
        value_start + 1
    } else {
        value_start
    };
    let value_end = command[digits_start..]
        .find(|character: char| !character.is_ascii_digit())
        .map_or(command.len(), |offset| digits_start + offset);
    if digits_start == value_end {
        return Err(format!("EZPL command {marker} has no numeric value"));
    }

    Ok(Some(format!(
        "{}{}{}",
        &command[..value_start],
        value,
        &command[value_end..]
    )))
}

fn replace_serial_start(command: &str, serial_number: u64) -> Result<String, String> {
    let (before_field, field) = command
        .split_once("C0,")
        .ok_or_else(|| "EZPL 207 row has no C0 serial field".to_owned())?;
    let field_width = field.bytes().take_while(u8::is_ascii_digit).count();
    if field_width == 0 {
        return Err("EZPL 207 C0 field has no numeric placeholder".to_owned());
    }

    let serial_text = serial_number.to_string();
    if serial_text.len() > field_width {
        return Err(format!(
            "serial number {serial_number} does not fit the {field_width}-digit EZPL 207 field"
        ));
    }

    Ok(format!(
        "{before_field}C0,{serial_number:0field_width$}{}",
        &field[field_width..]
    ))
}

#[cfg(test)]
fn ezpl_from_base64(
    encoded: &str,
    calculated_lot: &str,
    x_offset: i64,
    y_offset: i64,
    print_overrides: &PrintOverrides,
) -> Result<ProcessedMessage, Box<dyn std::error::Error>> {
    ezpl_from_base64_with_serial(
        encoded,
        calculated_lot,
        x_offset,
        y_offset,
        print_overrides,
        None,
        None,
    )
}

fn ezpl_from_base64_with_serial(
    encoded: &str,
    calculated_lot: &str,
    x_offset: i64,
    y_offset: i64,
    print_overrides: &PrintOverrides,
    serial_code: Option<&str>,
    serial_start: Option<u64>,
) -> Result<ProcessedMessage, Box<dyn std::error::Error>> {
    let decoded = STANDARD.decode(encoded.trim())?;
    let decoded = String::from_utf8(decoded)?;
    let mut commands = Vec::new();
    let mut minima = CoordinateMinima::default();
    let mut copies_applied = false;
    let mut temperature_applied = false;
    let mut speed_applied = false;
    let mut energy_applied = false;
    let mut layout_horizontal_applied = false;
    let mut layout_vertical_applied = false;
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

        if prefix == "225" {
            let serial_code = serial_code.ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "EZPL 225 row requires a generated serial code",
                )
            })?;
            commands.push(serial_code.to_owned());
        } else if prefix == "207" {
            if let Some(serial_start) = serial_start {
                commands.push(replace_serial_start(command, serial_start)?);
            } else {
                commands.push(command.to_owned());
            }
        } else if prefix == "210" {
            let Some((command, _old_lot)) = command.rsplit_once(',') else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "EZPL 210 row has no lot field",
                )
                .into());
            };
            commands.push(format!("{command},{calculated_lot}"));
        } else {
            let mut command = shift_row_coordinates(prefix, command, x_offset, y_offset)?;
            if prefix == "204" {
                if let Some(value) = print_overrides.copies {
                    if let Some(updated) =
                        replace_number_after_marker(&command, "^P", &value.to_string())?
                    {
                        command = updated;
                        copies_applied = true;
                    }
                }
            } else if prefix == "201" {
                if let Some(serial_start) = serial_start {
                    if let Some(updated) =
                        replace_number_after_marker(&command, "^C", &serial_start.to_string())?
                    {
                        command = updated;
                    }
                }
                if let Some(value) = print_overrides.temperature {
                    if let Some(updated) =
                        replace_number_after_marker(&command, "^H", &value.to_string())?
                    {
                        command = updated;
                        temperature_applied = true;
                    }
                }
                if let Some(value) = print_overrides.speed {
                    if let Some(updated) =
                        replace_number_after_marker(&command, "^S", &value.to_string())?
                    {
                        command = updated;
                        speed_applied = true;
                    }
                }
                if let Some(value) = print_overrides.stop_possition {
                    if let Some(updated) =
                        replace_number_after_marker(&command, "^E", &value.to_string())?
                    {
                        command = updated;
                        energy_applied = true;
                    }
                }
                if let Some(value) = print_overrides.layout_horizontal {
                    if let Some(updated) =
                        replace_number_after_marker(&command, "^R", &value.to_string())?
                    {
                        command = updated;
                        layout_horizontal_applied = true;
                    }
                }
                if let Some(value) = print_overrides.layout_vertical {
                    if let Some(updated) =
                        replace_number_after_marker(&command, "~Q", &value.to_string())?
                    {
                        command = updated;
                        layout_vertical_applied = true;
                    }
                }
            }
            commands.push(command);
        }
    }

    if (print_overrides.copies.is_some() && !copies_applied)
        || (print_overrides.temperature.is_some() && !temperature_applied)
        || (print_overrides.speed.is_some() && !speed_applied)
        || (print_overrides.stop_possition.is_some() && !energy_applied)
        || (print_overrides.layout_horizontal.is_some() && !layout_horizontal_applied)
        || (print_overrides.layout_vertical.is_some() && !layout_vertical_applied)
    {
        return Err("requested print setting was not found in the EZPL message".into());
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

fn append_success_log(
    config_path: &Path,
    date: NaiveDate,
    line: u32,
    args: &[String],
) -> io::Result<()> {
    let log_path = config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("ruprt.log");
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;

    writeln!(
        log,
        "{} date={} line={} args={args:?}",
        Local::now().format("%Y-%m-%d %H:%M:%S%:z"),
        date,
        line,
    )
}

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.is_empty() {
        print_minimal_help(
            "ruprt",
            env!("CARGO_PKG_VERSION"),
            "Loads and prepares a stored Godex EZPL print message.",
            "ruprt <project_ID> [-t [test]] [-l line] [-d YYMMDD] [-p count] [-h temp] [-s speed] [-e value] [-r value] [-q value] [-x offset] [-y offset]",
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

    let (config, config_path) = match AppConfig::load_with_path() {
        Ok(loaded) => loaded,
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
    let serial_lookup = match find_max_serial_number(&config.mysql_url, parsed.project_id, date) {
        Ok(result) => result,
        Err(error) => {
            eprintln!("Serial validation failed: {error}");
            process::exit(1);
        }
    };
    let serial_start = usable_serial_start(serial_lookup.is_serial, serial_lookup.max_serial);

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

    let serial_code = match message_has_prefix(&message, "225") {
        Ok(false) => None,
        Ok(true) => match generate_serial_code(&config.mysql_url, parsed.project_id, line) {
            Ok(serial_code) => Some(serial_code),
            Err(error) => {
                eprintln!("Serial code generation failed: {error}");
                process::exit(1);
            }
        },
        Err(error) => {
            eprintln!("Unable to inspect stored message: {error}");
            process::exit(1);
        }
    };

    match ezpl_from_base64_with_serial(
        &message,
        &calculated_lot,
        parsed.x_offset,
        parsed.y_offset,
        &parsed.print_overrides,
        serial_code.as_deref(),
        serial_start,
    ) {
        Ok(processed) => {
            if parsed.test_mode {
                print!("{}", processed.ezpl);
            } else {
                match send_to_printer(&config, processed.ezpl.as_bytes()) {
                    Ok(address) => {
                        eprintln!("Sent {} bytes to {address}", processed.ezpl.len());
                    }
                    Err(error) => {
                        eprintln!("Failed to send message to printer: {error}");
                        process::exit(1);
                    }
                }
            }

            if let Err(error) = append_success_log(&config_path, date, line, &args) {
                eprintln!("Failed to write ruprt.log: {error}");
                process::exit(1);
            }

            let _coordinate_minima = (processed.minima.x, processed.minima.y);
        }
        Err(error) => {
            eprintln!("Unable to decode stored message: {error}");
            process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Arguments, PrintOverrides, ezpl_from_base64, ezpl_from_base64_with_serial,
        message_has_prefix, message_template_line, parse_args, parse_yymmdd, usable_serial_start,
    };
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
    fn accepts_test_mode_with_or_without_the_test_word() {
        assert!(parse(&["628", "-t"]).unwrap().test_mode);
        assert!(parse(&["628", "-t", "test"]).unwrap().test_mode);
        assert!(!parse(&["628"]).unwrap().test_mode);
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
    fn accepts_print_override_ranges() {
        let parsed = parse(&["628", "-p", "1", "-h", "0", "-s", "2"]).unwrap();
        assert_eq!(parsed.print_overrides.copies, Some(1));
        assert_eq!(parsed.print_overrides.temperature, Some(0));
        assert_eq!(parsed.print_overrides.speed, Some(2));

        let parsed = parse(&["628", "-p", "20000", "-h", "20", "-s", "6"]).unwrap();
        assert_eq!(parsed.print_overrides.copies, Some(20000));
        assert_eq!(parsed.print_overrides.temperature, Some(20));
        assert_eq!(parsed.print_overrides.speed, Some(6));

        assert!(parse(&["628", "-p", "0"]).is_err());
        assert!(parse(&["628", "-p", "20001"]).is_err());
        assert!(parse(&["628", "-h", "21"]).is_err());
        assert!(parse(&["628", "-s", "1"]).is_err());
        assert!(parse(&["628", "-s", "7"]).is_err());
    }

    #[test]
    fn accepts_energy_and_layout_override_ranges() {
        let minimums = parse(&["628", "-e", "-40", "-r", "0", "-q", "-100"]).unwrap();
        assert_eq!(minimums.print_overrides.stop_possition, Some(-40));
        assert_eq!(minimums.print_overrides.layout_horizontal, Some(0));
        assert_eq!(minimums.print_overrides.layout_vertical, Some(-100));

        let maximums = parse(&["628", "-e", "40", "-r", "100", "-q", "100"]).unwrap();
        assert_eq!(maximums.print_overrides.stop_possition, Some(40));
        assert_eq!(maximums.print_overrides.layout_horizontal, Some(100));
        assert_eq!(maximums.print_overrides.layout_vertical, Some(100));

        assert!(parse(&["628", "-e", "-41"]).is_err());
        assert!(parse(&["628", "-e", "41"]).is_err());
        assert!(parse(&["628", "-r", "101"]).is_err());
        assert!(parse(&["628", "-q", "-101"]).is_err());
        assert!(parse(&["628", "-q", "101"]).is_err());
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
    fn only_positive_serialized_results_set_serial_start() {
        assert_eq!(usable_serial_start(false, 123), None);
        assert_eq!(usable_serial_start(true, 0), None);
        assert_eq!(usable_serial_start(true, 123), Some(123));
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
            ezpl_from_base64_with_serial(
                &message,
                "LOT42",
                0,
                0,
                &PrintOverrides::default(),
                Some("T88792^C0511619962"),
                None,
            )
            .unwrap()
            .ezpl,
            "^H15\nT88792^C0511619962\n"
        );
    }

    #[test]
    fn calls_for_a_serial_code_only_when_message_has_225_row() {
        let with_qr = STANDARD.encode("201||^E\n225||old-code\n");
        let without_qr = STANDARD.encode("201||^E\n");
        assert!(message_has_prefix(&with_qr, "225").unwrap());
        assert!(!message_has_prefix(&without_qr, "225").unwrap());
    }

    #[test]
    fn replaces_the_entire_225_row_with_generated_serial_code() {
        let message = STANDARD.encode("225||CZ4475004970#10042015#^C0\n201||^E\n");
        let result = ezpl_from_base64_with_serial(
            &message,
            "LOT42",
            0,
            0,
            &PrintOverrides::default(),
            Some("T88792C0F126279^C0511619962"),
            None,
        )
        .unwrap();
        assert_eq!(result.ezpl, "T88792C0F126279^C0511619962\n^E\n");

        assert!(ezpl_from_base64(&message, "LOT42", 0, 0, &PrintOverrides::default()).is_err());
    }

    #[test]
    fn replaces_serial_start_in_201_c0_and_preserves_it_without_a_start() {
        let message = STANDARD.encode("201||^C0\n201||^E0\n");
        let with_serial = ezpl_from_base64_with_serial(
            &message,
            "LOT42",
            0,
            0,
            &PrintOverrides::default(),
            None,
            Some(12345),
        )
        .unwrap();
        assert_eq!(with_serial.ezpl, "^C12345\n^E0\n");

        let without_serial = ezpl_from_base64_with_serial(
            &message,
            "LOT42",
            0,
            0,
            &PrintOverrides::default(),
            None,
            None,
        )
        .unwrap();
        assert_eq!(without_serial.ezpl, "^C0\n^E0\n");
    }

    #[test]
    fn replaces_the_text_after_the_last_comma_in_the_210_row() {
        let message = STANDARD.encode("210||AD,0035,0245,1,1,0,0,OLD-LOT\n201||^E\n");
        assert_eq!(
            ezpl_from_base64(&message, "NEW-LOT", 0, 0, &PrintOverrides::default())
                .unwrap()
                .ezpl,
            "AD,0035,0245,1,1,0,0,NEW-LOT\n^E\n"
        );
    }

    #[test]
    fn stores_the_smallest_coordinates_from_231_and_232_rows() {
        let message =
            STANDARD.encode("231||AD,0035,0245,1,1\n232||W0300,0045,3,2\n232||XRB0020,0030,6,2\n");
        let processed =
            ezpl_from_base64(&message, "LOT42", 0, 0, &PrintOverrides::default()).unwrap();

        assert_eq!(processed.minima.x, Some(20));
        assert_eq!(processed.minima.y, Some(30));
    }

    #[test]
    fn applies_signed_offsets_and_clamps_coordinates_to_zero() {
        let message =
            STANDARD.encode("231||AD,0035,0245,1,1\n232||W0300,0045,3,2\n232||XRB0020,0030,6,2\n");
        let processed =
            ezpl_from_base64(&message, "LOT42", -50, 10, &PrintOverrides::default()).unwrap();

        assert_eq!(
            processed.ezpl,
            "AD,0000,0255,1,1\nW0250,0055,3,2\nXRB0000,0040,6,2\n"
        );
    }

    #[test]
    fn clamps_negative_coordinate_results_to_zero() {
        let message = STANDARD.encode("231||AD,0005,0003,1,1\n");
        let processed =
            ezpl_from_base64(&message, "LOT42", -10, -10, &PrintOverrides::default()).unwrap();
        assert_eq!(processed.ezpl, "AD,0000,0000,1,1\n");
    }

    #[test]
    fn replaces_copy_temperature_and_speed_commands() {
        let message = STANDARD.encode("204||^P100\n201||^H15\n201||^S2\n");
        let overrides = PrintOverrides {
            copies: Some(20000),
            temperature: Some(0),
            speed: Some(6),
            ..PrintOverrides::default()
        };
        let processed = ezpl_from_base64(&message, "LOT42", 0, 0, &overrides).unwrap();
        assert_eq!(processed.ezpl, "^P20000\n^H0\n^S6\n");
    }

    #[test]
    fn replaces_energy_and_layout_commands() {
        let message = STANDARD.encode("201||^E0\n201||^R50\n201||~Q0\n");
        let overrides = PrintOverrides {
            stop_possition: Some(-40),
            layout_horizontal: Some(100),
            layout_vertical: Some(-100),
            ..PrintOverrides::default()
        };
        let processed = ezpl_from_base64(&message, "LOT42", 0, 0, &overrides).unwrap();
        assert_eq!(processed.ezpl, "^E-40\n^R100\n~Q-100\n");
    }
}
