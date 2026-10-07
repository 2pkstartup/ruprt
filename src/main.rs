//! Loads EZPL from the database or a `.clf` file, prepares a print copy, and
//! either prints it in test mode or sends it to the configured printer.
//! File-based prints also save a zeroed-serial copy into the monthly autosave table.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::{Local, NaiveDate, NaiveDateTime};
use ruprt::{
    config::AppConfig,
    db::{
        calculate_project_lot, find_max_serial_number, generate_serial_code, latest_message,
        save_autosave_message,
    },
    print_minimal_help,
    printer::send_to_printer,
};
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    process,
};

struct Arguments {
    /// Optional because a CLF can provide the project in its `008||` row.
    project_id: Option<u32>,
    /// When present, use this CLF instead of looking up a template in MySQL.
    input_clf: Option<PathBuf>,
    /// Requested production line; the config supplies a default when omitted.
    line: Option<u32>,
    /// Date used by project lot and serial lookup; defaults to today's local date.
    date: Option<NaiveDate>,
    /// Export the stored database message without print processing.
    raw_export: bool,
    /// Signed shifts applied only to the first two coordinate numbers in 231/232.
    x_offset: i64,
    y_offset: i64,
    /// Print to stdout instead of sending to the printer; never creates autosave.
    test_mode: bool,
    /// Skip the autosave insert after printing a file.
    skip_autosave: bool,
    /// Explicit changes to print settings; absent values preserve template settings.
    print_overrides: PrintOverrides,
}

/// Optional EZPL values that are replaced only when their corresponding flag is given.
#[derive(Debug, Default, PartialEq, Eq)]
struct PrintOverrides {
    copies: Option<u32>,
    temperature: Option<u32>,
    speed: Option<u32>,
    stop_possition: Option<i32>,
    layout_horizontal: Option<u32>,
    layout_vertical: Option<i32>,
}

/// Original smallest X/Y positions found in the message before applying offsets.
#[derive(Debug, Default, PartialEq, Eq)]
struct CoordinateMinima {
    x: Option<i32>,
    y: Option<i32>,
}

/// Both outputs from one transformation: the printed copy and its autosave copy.
/// The autosave form restores serial-start placeholders to zero.
#[derive(Debug, PartialEq, Eq)]
struct ProcessedMessage {
    /// Prefix-stripped, transformed EZPL bytes represented as UTF-8 text.
    ezpl: String,
    /// Base64 of the transformed source rows, with serial-start fields zeroed.
    autosave_base64: String,
    minima: CoordinateMinima,
}

/// Lines 2, 3, and 4 share the line-2 message template; keep the requested line
/// separately because serial generation and autosave still use the real line.
fn message_template_line(requested_line: u32) -> u32 {
    if (2..=4).contains(&requested_line) {
        2
    } else {
        requested_line
    }
}

/// Returns no serial start for non-serial projects or an empty history.
/// Otherwise advances to the next hundred so the current print starts a fresh block.
fn usable_serial_start(is_serial: bool, serial_number: u64) -> Option<u64> {
    if !is_serial || serial_number == 0 {
        return None;
    }

    serial_number.div_ceil(100).checked_mul(100)
}

/// Formats a positive start number to the project's configured fixed SN width.
fn format_serial_start(serial_number: u64, digit_count: Option<u32>) -> Result<String, String> {
    let width = digit_count
        .filter(|width| *width > 0)
        .ok_or_else(|| "project has no valid serial digit_count".to_owned())?
        as usize;
    let digits = serial_number.to_string();
    if digits.len() > width {
        return Err(format!(
            "serial start {serial_number} does not fit the configured {width}-digit field"
        ));
    }

    Ok(format!("{serial_number:0width$}"))
}

/// Parses a project ID or CLF path followed by non-repeating flag/value pairs.
/// A project ID in a CLF is reconciled with the optional command-line ID later.
fn parse_args(args: &[String]) -> Result<Arguments, String> {
    let usage = "Usage: ruprt <project_ID|message.clf> [message.clf] [-v] [--raw] [-t [test]] [-l line] [-d YYMMDD] [-p count] [-h temp] [-s speed] [-e value] [-r value] [-q value] [-x offset] [-y offset]";
    let first = args.first().ok_or_else(|| usage.to_owned())?;
    let project_id = first.parse::<u32>().ok();
    if project_id.is_some_and(|id| id > 5000) {
        return Err("project_ID must be an integer from 0 to 5000".to_owned());
    }
    let mut input_clf = None;
    let mut index = 1;
    if project_id.is_none() {
        if Path::new(first)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("clf"))
        {
            input_clf = Some(PathBuf::from(first));
        } else {
            return Err(usage.to_owned());
        }
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
    let mut raw_export = false;
    let mut skip_autosave = false;
    while index < args.len() {
        if args[index] == "-v" && !skip_autosave {
            skip_autosave = true;
            index += 1;
            continue;
        }
        if input_clf.is_none()
            && Path::new(&args[index])
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("clf"))
        {
            input_clf = Some(PathBuf::from(&args[index]));
            index += 1;
            continue;
        }

        if args[index] == "--raw" && !raw_export {
            raw_export = true;
            index += 1;
            continue;
        }

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
                return Err(usage.to_owned());
            }
        }
        index += 2;
    }

    if raw_export && input_clf.is_some() {
        return Err("--raw cannot be combined with a .clf input file".to_owned());
    }
    if skip_autosave && input_clf.is_none() {
        return Err("-v requires a .clf input file".to_owned());
    }

    Ok(Arguments {
        project_id,
        input_clf,
        line,
        date,
        raw_export,
        x_offset: x_offset.unwrap_or(0),
        y_offset: y_offset.unwrap_or(0),
        test_mode,
        skip_autosave,
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

/// Parses an unsigned setting and enforces its inclusive supported range.
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

/// Parses a signed setting and enforces its inclusive supported range.
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

/// Interprets two-digit years as 2000-2099 and rejects invalid calendar dates.
fn parse_yymmdd(value: &str) -> Result<NaiveDate, String> {
    if value.len() != 6 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("date must use YYMMDD format".to_owned());
    }

    let year = 2000 + value[..2].parse::<i32>().map_err(|_| "invalid date")?;
    let month = value[2..4].parse::<u32>().map_err(|_| "invalid date")?;
    let day = value[4..6].parse::<u32>().map_err(|_| "invalid date")?;
    NaiveDate::from_ymd_opt(year, month, day).ok_or_else(|| "invalid date".to_owned())
}

/// Decodes the stored message without stripping EZPL source-row prefixes.
fn decode_raw_message(encoded: &str) -> Result<Vec<u8>, base64::DecodeError> {
    STANDARD.decode(encoded.trim())
}

/// Converts a local name into one safe filename component without path separators.
fn local_name_component(local_name: &str) -> Option<String> {
    let mut component = String::new();
    let mut previous_was_separator = false;

    for character in local_name.trim().chars() {
        let character = if character.is_whitespace()
            || character.is_control()
            || matches!(
                character,
                '/' | '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*'
            ) {
            '-'
        } else {
            character
        };

        if character == '-' {
            if !previous_was_separator {
                component.push(character);
            }
            previous_was_separator = true;
        } else {
            component.push(character);
            previous_was_separator = false;
        }
    }

    let component = component.trim_matches(['-', '.']).to_owned();
    (!component.is_empty()).then_some(component)
}

/// Reads the raw `009||` value, if it exists, without applying filename rules.
fn raw_message_optional_local_name(
    decoded: &[u8],
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let decoded = std::str::from_utf8(decoded)?;
    let local_name = decoded
        .lines()
        .find_map(|row| {
            let (prefix, value) = row.split_once("||")?;
            (prefix == "009").then_some(value)
        })
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    Ok(local_name)
}

/// Requires a nonempty `009||` value for database-message export filenames.
fn raw_message_local_name_value(decoded: &[u8]) -> Result<String, Box<dyn std::error::Error>> {
    raw_message_optional_local_name(decoded)?
        .ok_or_else(|| "raw message has no usable 009 local_name".into())
}

/// Returns the sanitized local name used by raw export filenames.
fn raw_message_local_name(decoded: &[u8]) -> Result<String, Box<dyn std::error::Error>> {
    let local_name = raw_message_local_name_value(decoded)?;
    local_name_component(&local_name).ok_or_else(|| "raw message has an empty local_name".into())
}

/// Reads project ID from the CLF's `008||` row; absence is not itself an error.
fn raw_message_project_id(decoded: &[u8]) -> Result<Option<u32>, Box<dyn std::error::Error>> {
    let decoded = std::str::from_utf8(decoded)?;
    decoded
        .lines()
        .find_map(|row| {
            let (prefix, value) = row.split_once("||")?;
            (prefix == "008").then_some(value.trim())
        })
        .map(|value| {
            let project_id = value.parse::<u32>()?;
            if project_id > 5000 {
                return Err("project_ID in CLF must be from 0 to 5000".into());
            }
            Ok(project_id)
        })
        .transpose()
}

/// Loads CLF bytes, resolves project ID, and returns Base64 plus the raw local name.
fn load_clf_input(
    path: &Path,
    argument_project_id: Option<u32>,
) -> Result<(u32, String, Option<String>), Box<dyn std::error::Error>> {
    let decoded = fs::read(path)?;
    let project_id = resolve_project_id(argument_project_id, raw_message_project_id(&decoded)?)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let local_name = raw_message_optional_local_name(&decoded)?.unwrap_or_else(|| "N/A".to_owned());
    Ok((project_id, STANDARD.encode(decoded), Some(local_name)))
}

/// Uses the CLI project ID when supplied, but rejects disagreement with row 008.
fn resolve_project_id(
    argument_project_id: Option<u32>,
    message_project_id: Option<u32>,
) -> Result<u32, String> {
    match (argument_project_id, message_project_id) {
        (Some(argument), Some(message)) if argument != message => Err(format!(
            "project_ID {argument} does not match CLF project_ID {message}"
        )),
        (Some(argument), _) => Ok(argument),
        (None, Some(message)) => Ok(message),
        (None, None) => Err("project_ID is missing from arguments and CLF row 008".to_owned()),
    }
}

/// Builds the non-overwriting filename used by `--raw` exports.
fn raw_export_filename(project_id: u32, local_name: &str, timestamp: NaiveDateTime) -> String {
    format!(
        "{project_id}_{local_name}_{}.clf",
        timestamp.format("%y-%m-%d_%H:%M:%S")
    )
}

/// Checks whether a stored message contains a command row with the given prefix.
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

/// Finds all contiguous ASCII digit ranges; for 231/232 the first two are X/Y.
fn coordinate_spans(command: &str) -> Vec<(usize, usize)> {
    let bytes = command.as_bytes();
    let mut spans = Vec::new();
    let mut index = 0;

    while index < bytes.len() {
        if !bytes[index].is_ascii_digit() {
            index += 1;
            continue;
        }

        let start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        spans.push((start, index));
    }

    spans
}

/// Reads X/Y from the first two numeric groups in either coordinate-row format.
fn coordinates_for_row(prefix: &str, command: &str) -> Option<(i32, i32)> {
    if prefix != "231" && prefix != "232" {
        return None;
    }

    let spans = coordinate_spans(command);
    let (x_start, x_end) = *spans.first()?;
    let (y_start, y_end) = *spans.get(1)?;
    Some((
        command[x_start..x_end].parse().ok()?,
        command[y_start..y_end].parse().ok()?,
    ))
}

/// Updates independent minima from the original coordinates in a message.
fn update_minima(minima: &mut CoordinateMinima, x: i32, y: i32) {
    minima.x = Some(minima.x.map_or(x, |current| current.min(x)));
    minima.y = Some(minima.y.map_or(y, |current| current.min(y)));
}

/// Applies one signed offset, clamps at zero, and retains the input digit width.
fn shift_coordinate(value: &str, offset: i64) -> Result<String, String> {
    let coordinate = value
        .parse::<i64>()
        .map_err(|_| "coordinate must be a non-negative integer".to_owned())?;
    let shifted = coordinate.saturating_add(offset).max(0);
    Ok(format!("{shifted:0width$}", width = value.len()))
}

/// Shifts only the first two numeric groups of 231/232 and preserves all remaining bytes.
fn shift_row_coordinates(
    prefix: &str,
    command: &str,
    x_offset: i64,
    y_offset: i64,
) -> Result<String, String> {
    if x_offset == 0 && y_offset == 0 {
        return Ok(command.to_owned());
    }

    if prefix != "231" && prefix != "232" {
        return Ok(command.to_owned());
    }

    let spans = coordinate_spans(command);
    if spans.len() < 2 {
        return Err(format!("EZPL {prefix} row has no X/Y coordinates"));
    }

    let mut shifted = command.to_owned();
    for ((start, end), offset) in spans.into_iter().take(2).zip([x_offset, y_offset]).rev() {
        let value = shift_coordinate(&command[start..end], offset)?;
        shifted.replace_range(start..end, &value);
    }
    Ok(shifted)
}

/// Replaces the first integer after an EZPL marker while preserving its suffix.
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

/// Replaces the 207 C0 serial field without changing the rest of that command.
fn replace_serial_start(command: &str, serial_number: &str) -> Result<String, String> {
    let (before_field, field) = command
        .split_once("C0,")
        .ok_or_else(|| "EZPL 207 row has no C0 serial field".to_owned())?;
    let field_width = field.bytes().take_while(u8::is_ascii_digit).count();
    if field_width == 0 {
        return Err("EZPL 207 C0 field has no numeric placeholder".to_owned());
    }

    if serial_number.is_empty() || !serial_number.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("serial start must contain only digits".to_owned());
    }
    if serial_number.len() > field_width {
        return Err(format!(
            "serial start {serial_number} does not fit the {field_width}-digit EZPL 207 field"
        ));
    }

    let padding = "0".repeat(field_width - serial_number.len());
    Ok(format!(
        "{before_field}C0,{padding}{serial_number}{}",
        &field[field_width..]
    ))
}

/// Zeros the numeric field after a marker while keeping its existing width.
fn zero_number_after_marker(command: &str, marker: &str) -> Result<Option<String>, String> {
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
    let width = value_end - digits_start;
    if width == 0 {
        return Err(format!("EZPL command {marker} has no numeric value"));
    }

    Ok(Some(
        replace_number_after_marker(command, marker, &"0".repeat(width))?
            .expect("marker was checked above"),
    ))
}

/// Ensures every autosave has project/name metadata, using documented fallbacks.
fn ensure_autosave_metadata(rows: &mut Vec<String>) {
    let has_project_id = rows.iter().any(|row| row.starts_with("008||"));
    let has_local_name = rows.iter().any(|row| row.starts_with("009||"));
    let mut metadata = Vec::new();
    if !has_project_id {
        metadata.push("008||0000".to_owned());
    }
    if !has_local_name {
        metadata.push("009||N/A".to_owned());
    }
    metadata.append(rows);
    *rows = metadata;
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

/// Decodes a template once and builds both its printed EZPL and autosave form.
/// It strips source prefixes, applies explicit overrides, inserts lot/serial data,
/// and resets only the autosave serial-start fields to their zero placeholders.
fn ezpl_from_base64_with_serial(
    encoded: &str,
    calculated_lot: &str,
    x_offset: i64,
    y_offset: i64,
    print_overrides: &PrintOverrides,
    serial_code: Option<&str>,
    serial_start: Option<&str>,
) -> Result<ProcessedMessage, Box<dyn std::error::Error>> {
    process_message(
        encoded,
        calculated_lot,
        x_offset,
        y_offset,
        print_overrides,
        serial_code,
        serial_start,
        false,
    )
}

fn process_message(
    encoded: &str,
    calculated_lot: &str,
    x_offset: i64,
    y_offset: i64,
    print_overrides: &PrintOverrides,
    serial_code: Option<&str>,
    serial_start: Option<&str>,
    preserve_file_values: bool,
) -> Result<ProcessedMessage, Box<dyn std::error::Error>> {
    let decoded = STANDARD.decode(encoded.trim())?;
    let decoded = String::from_utf8(decoded)?;
    let mut commands = Vec::new();
    let mut autosave_rows = Vec::new();
    let mut minima = CoordinateMinima::default();
    let mut copies_applied = false;
    let mut temperature_applied = false;
    let mut speed_applied = false;
    let mut energy_applied = false;
    let mut layout_horizontal_applied = false;
    let mut layout_vertical_applied = false;
    for row in decoded.lines() {
        let Some((prefix, command)) = row.split_once("||") else {
            autosave_rows.push(row.to_owned());
            continue;
        };
        if prefix == "200" {
            continue;
        }
        if let Some((x, y)) = coordinates_for_row(prefix, command) {
            update_minima(&mut minima, x, y);
        }
        if prefix.len() != 3
            || !prefix.starts_with('2')
            || !prefix.bytes().all(|byte| byte.is_ascii_digit())
        {
            autosave_rows.push(row.to_owned());
            continue;
        }

        if prefix == "225" {
            let serial_code = if preserve_file_values {
                command
            } else {
                serial_code.ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "EZPL 225 row requires a generated serial code",
                    )
                })?
            };
            commands.push(serial_code.to_owned());
            autosave_rows.push(format!("{prefix}||{serial_code}"));
        } else if prefix == "207" {
            let printed_command = match serial_start {
                Some(serial_start) => replace_serial_start(command, serial_start)?,
                None => command.to_owned(),
            };
            let saved_command = replace_serial_start(command, "0")?;
            commands.push(printed_command);
            autosave_rows.push(format!("{prefix}||{saved_command}"));
        } else if prefix == "210" {
            if preserve_file_values {
                commands.push(command.to_owned());
                autosave_rows.push(row.to_owned());
                continue;
            }
            let Some((command, _old_lot)) = command.rsplit_once(',') else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "EZPL 210 row has no lot field",
                )
                .into());
            };
            let updated = format!("{command},{calculated_lot}");
            commands.push(updated.clone());
            autosave_rows.push(format!("{prefix}||{updated}"));
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
                        replace_number_after_marker(&command, "^C", serial_start)?
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
            let saved_command = if prefix == "201" {
                zero_number_after_marker(&command, "^C")?.unwrap_or_else(|| command.clone())
            } else {
                command.clone()
            };
            if prefix == "201" && serial_start.is_some() {
                if let Some(updated) =
                    replace_number_after_marker(&command, "^C", serial_start.unwrap())?
                {
                    commands.push(updated);
                } else {
                    commands.push(command);
                }
            } else {
                commands.push(command);
            }
            autosave_rows.push(format!("{prefix}||{saved_command}"));
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

    ensure_autosave_metadata(&mut autosave_rows);

    let mut commands = commands.join("\n");
    if !commands.is_empty() {
        commands.push('\n');
    }
    let mut autosave_message = autosave_rows.join("\n");
    if !autosave_message.is_empty() {
        autosave_message.push('\n');
    }
    Ok(ProcessedMessage {
        ezpl: commands,
        autosave_base64: STANDARD.encode(autosave_message.as_bytes()),
        minima,
    })
}

/// Appends a successful print's input arguments and effective date/link to the log.
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

/// Prints a self-contained CLF without consulting project templates or procedures.
fn run_clf(
    parsed: &Arguments,
    config: &AppConfig,
    config_path: &Path,
    args: &[String],
    path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let decoded = fs::read(path)?;
    let embedded_id = raw_message_project_id(&decoded)?;
    let project_id = if parsed.project_id.is_none() && embedded_id.is_none() {
        0
    } else {
        resolve_project_id(parsed.project_id, embedded_id)?
    };
    let name = raw_message_optional_local_name(&decoded)?.unwrap_or_else(|| "N/A".to_owned());
    let line = parsed.line.or(config.default_line).unwrap_or(0);
    let printer_id = if !parsed.skip_autosave && !parsed.test_mode {
        Some(
            config
                .printer_id
                .ok_or("printer_id is required for CLF autosave")?,
        )
    } else {
        None
    };
    let processed = process_message(
        &STANDARD.encode(decoded),
        "",
        parsed.x_offset,
        parsed.y_offset,
        &parsed.print_overrides,
        None,
        None,
        true,
    )?;
    if parsed.test_mode {
        print!("{}", processed.ezpl);
    } else {
        let address = send_to_printer(config, processed.ezpl.as_bytes())?;
        eprintln!("Sent {} bytes to {address}", processed.ezpl.len());
        if let Some(printer_id) = printer_id {
            save_autosave_message(
                &config.mysql_url,
                project_id,
                &name,
                line,
                printer_id,
                &processed.autosave_base64,
            )
            .map_err(|error| {
                format!("Message sent, but autosave failed (do not reprint automatically): {error}")
            })?;
        }
    }
    append_success_log(
        config_path,
        parsed.date.unwrap_or_else(|| Local::now().date_naive()),
        line,
        args,
    )?;
    Ok(())
}

/// Orchestrates input selection, always-on serial validation, transform, and output.
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.is_empty() {
        print_minimal_help(
            "ruprt",
            env!("CARGO_PKG_VERSION"),
            "Loads and prepares a stored Godex EZPL print message.",
            "ruprt <project_ID|message.clf> [message.clf] [-v] [--raw] [-t [test]] [-l line] [-d YYMMDD] [-p count] [-h temp] [-s speed] [-e value] [-r value] [-q value] [-x offset] [-y offset]",
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
    if let Some(path) = parsed.input_clf.as_ref() {
        if let Err(error) = run_clf(&parsed, &config, &config_path, &args, path) {
            eprintln!("CLF print failed: {error}");
            process::exit(1);
        }
        return;
    }
    let line = match parsed.line.or(config.default_line) {
        Some(line) => line,
        None => {
            eprintln!("Specify -l line or set default_line in config.toml");
            process::exit(2);
        }
    };

    // A CLF is the complete message source; this branch deliberately avoids tbl_mess.
    let (project_id, message, file_local_name) = if let Some(path) = parsed.input_clf.as_ref() {
        match load_clf_input(path, parsed.project_id) {
            Ok(input) => input,
            Err(error) => {
                eprintln!("Unable to load CLF input: {error}");
                process::exit(1);
            }
        }
    } else {
        let project_id = match parsed.project_id {
            Some(project_id) => project_id,
            None => {
                eprintln!("Specify project_ID or use a CLF containing row 008||");
                process::exit(2);
            }
        };
        // Keep `line` unchanged for later EZPL edits; only template lookup is shared.
        let message_line = message_template_line(line);
        let message = match latest_message(&config.mysql_url, project_id, message_line) {
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
        (project_id, message, None)
    };

    let autosave_printer_id = if file_local_name.is_some() && !parsed.test_mode {
        Some(match config.printer_id {
            Some(printer_id) => printer_id,
            None => {
                eprintln!("printer_id is required in config.toml to autosave a CLF print");
                process::exit(2);
            }
        })
    } else {
        None
    };

    if parsed.raw_export {
        let decoded = match decode_raw_message(&message) {
            Ok(message) => message,
            Err(error) => {
                eprintln!("Unable to decode stored message: {error}");
                process::exit(1);
            }
        };
        let local_name = match raw_message_local_name(&decoded) {
            Ok(local_name) => local_name,
            Err(error) => {
                eprintln!("Unable to read local_name from message: {error}");
                process::exit(1);
            }
        };
        let filename = raw_export_filename(project_id, &local_name, Local::now().naive_local());
        let path = match std::env::current_dir() {
            Ok(directory) => directory.join(filename),
            Err(error) => {
                eprintln!("Unable to determine output directory: {error}");
                process::exit(1);
            }
        };
        let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => file,
            Err(error) => {
                eprintln!("Unable to create {}: {error}", path.display());
                process::exit(1);
            }
        };
        if let Err(error) = file.write_all(&decoded) {
            eprintln!("Unable to write {}: {error}", path.display());
            process::exit(1);
        }
        println!("{}", path.display());
        return;
    }

    let date = parsed.date.unwrap_or_else(|| Local::now().date_naive());
    // Validate serial history even if the template has no serial field.
    let serial_lookup = match find_max_serial_number(&config.mysql_url, project_id, date) {
        Ok(result) => result,
        Err(error) => {
            eprintln!("Serial validation failed: {error}");
            process::exit(1);
        }
    };
    let serial_start_number =
        usable_serial_start(serial_lookup.is_serial, serial_lookup.max_serial);
    let serial_start = match serial_start_number {
        Some(serial_start) => match format_serial_start(serial_start, serial_lookup.digit_count) {
            Ok(serial_start) => Some(serial_start),
            Err(error) => {
                eprintln!("Unable to format serial start: {error}");
                process::exit(1);
            }
        },
        None => None,
    };

    let calculated_lot = match calculate_project_lot(&config.mysql_url, project_id, date) {
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

    let serial_code = match message_has_prefix(&message, "225") {
        Ok(false) => None,
        Ok(true) => match generate_serial_code(&config.mysql_url, project_id, line) {
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

    // Sending must succeed before a file-based message is committed as autosave.
    match ezpl_from_base64_with_serial(
        &message,
        &calculated_lot,
        parsed.x_offset,
        parsed.y_offset,
        &parsed.print_overrides,
        serial_code.as_deref(),
        serial_start.as_deref(),
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

                if let (Some(name), Some(printer_id)) =
                    (file_local_name.as_deref(), autosave_printer_id)
                {
                    if let Err(error) = save_autosave_message(
                        &config.mysql_url,
                        project_id,
                        name,
                        line,
                        printer_id,
                        &processed.autosave_base64,
                    ) {
                        eprintln!("Printed message but failed to save autosave row: {error}");
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
        Arguments, PrintOverrides, decode_raw_message, ezpl_from_base64,
        ezpl_from_base64_with_serial, format_serial_start, message_has_prefix,
        message_template_line, parse_args, parse_yymmdd, raw_export_filename,
        raw_message_local_name, raw_message_local_name_value, raw_message_optional_local_name,
        raw_message_project_id, resolve_project_id, usable_serial_start,
    };
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use chrono::NaiveDate;
    use std::path::PathBuf;

    #[test]
    fn accepts_file_only_and_optional_no_autosave_flag() {
        let parsed = parse(&["label.clf"]).unwrap();
        assert_eq!(parsed.project_id, None);
        assert_eq!(parsed.line, None);
        assert!(!parsed.skip_autosave);
        assert!(parse(&["label.clf", "-v"]).unwrap().skip_autosave);
        assert!(parse(&["945", "-v"]).is_err());
    }

    #[test]
    fn file_print_preserves_lot_qr_and_serial_without_project_metadata() {
        let encoded = STANDARD.encode(
            "200||comment\n210||AT,0010,0020,OLDLOT\n225||QR^C0\n207||C0,00123,+1,A1\n201||E\n",
        );
        let result = super::process_message(
            &encoded,
            "",
            0,
            0,
            &PrintOverrides::default(),
            None,
            None,
            true,
        )
        .unwrap();
        assert_eq!(
            result.ezpl,
            "AT,0010,0020,OLDLOT\nQR^C0\nC0,00123,+1,A1\nE\n"
        );
        let saved = String::from_utf8(STANDARD.decode(result.autosave_base64).unwrap()).unwrap();
        assert!(saved.starts_with("008||0000\n009||N/A\n"));
        assert!(saved.contains("207||C0,00000,+1,A1"));
    }

    fn parse(values: &[&str]) -> Result<Arguments, String> {
        let args = values
            .iter()
            .map(|value| (*value).to_owned())
            .collect::<Vec<_>>();
        parse_args(&args)
    }

    #[test]
    fn accepts_project_with_optional_line_override() {
        assert_eq!(parse(&["628"]).unwrap().project_id, Some(628));
        assert_eq!(parse(&["628", "-l", "3"]).unwrap().line, Some(3));
    }

    #[test]
    fn accepts_clf_as_input_with_or_without_project_argument() {
        let with_project = parse(&["945", "label.clf", "-l", "1"]).unwrap();
        assert_eq!(with_project.project_id, Some(945));
        assert_eq!(with_project.input_clf, Some(PathBuf::from("label.clf")));

        let from_file = parse(&["label.clf", "-l", "1"]).unwrap();
        assert_eq!(from_file.project_id, None);
        assert_eq!(from_file.input_clf, Some(PathBuf::from("label.clf")));
    }

    #[test]
    fn resolves_project_id_from_argument_or_008_row() {
        let decoded = b"001||01\n008||945\n009||Local Name\n";
        let embedded = raw_message_project_id(decoded).unwrap();
        assert_eq!(embedded, Some(945));
        assert_eq!(resolve_project_id(None, embedded).unwrap(), 945);
        assert_eq!(resolve_project_id(Some(945), embedded).unwrap(), 945);
        assert!(resolve_project_id(Some(628), embedded).is_err());
        assert!(resolve_project_id(None, None).is_err());
        assert!(raw_message_project_id(b"008||5001\n").is_err());
    }

    #[test]
    fn autosave_payload_keeps_print_overrides_and_zeroes_serial_start() {
        let message = STANDARD
            .encode("008||945\n009||Demo Label\n207||C0,00000,+1,A1\n201||^C0\n204||^P100\n");
        let processed = ezpl_from_base64_with_serial(
            &message,
            "LOT42",
            0,
            0,
            &PrintOverrides {
                copies: Some(3),
                ..PrintOverrides::default()
            },
            None,
            Some("01234"),
        )
        .unwrap();

        let saved = String::from_utf8(STANDARD.decode(processed.autosave_base64).unwrap()).unwrap();
        assert!(saved.starts_with("008||945\n009||Demo Label\n"));
        assert!(saved.contains("207||C0,00000,+1,A1"));
        assert!(saved.contains("201||^C0"));
        assert!(saved.contains("204||^P3"));
    }

    #[test]
    fn inserts_default_008_and_009_rows_when_clf_lacks_them() {
        let message = STANDARD.encode("201||^E\n");
        let processed =
            ezpl_from_base64(&message, "LOT42", 0, 0, &PrintOverrides::default()).unwrap();
        let saved = String::from_utf8(STANDARD.decode(processed.autosave_base64).unwrap()).unwrap();
        assert!(saved.starts_with("008||0000\n009||N/A\n"));
        assert!(processed.ezpl.starts_with("^E\n"));
    }

    #[test]
    fn accepts_test_mode_with_or_without_the_test_word() {
        assert!(parse(&["628", "-t"]).unwrap().test_mode);
        assert!(parse(&["628", "-t", "test"]).unwrap().test_mode);
        assert!(!parse(&["628"]).unwrap().test_mode);
    }

    #[test]
    fn accepts_raw_export_flag() {
        assert!(parse(&["628", "--raw"]).unwrap().raw_export);
        assert!(parse(&["628", "-l", "1", "--raw"]).unwrap().raw_export);
        assert!(!parse(&["628"]).unwrap().raw_export);
    }

    #[test]
    fn decodes_raw_message_without_ezpl_processing() {
        let encoded = STANDARD.encode(b"201||^C0\n225||raw\n");
        assert_eq!(
            decode_raw_message(&encoded).unwrap(),
            b"201||^C0\n225||raw\n"
        );
    }

    #[test]
    fn raw_export_filename_uses_project_and_local_timestamp_format() {
        let timestamp = NaiveDate::from_ymd_opt(2026, 10, 6)
            .unwrap()
            .and_hms_opt(9, 8, 7)
            .unwrap();
        assert_eq!(
            raw_export_filename(945, "Name-With-Spaces", timestamp),
            "945_Name-With-Spaces_26-10-06_09:08:07.clf"
        );
    }

    #[test]
    fn raw_export_uses_sanitized_009_local_name() {
        let message = b"001||01\n009||GGV 11/12: TEST\n201||^E\n";
        assert_eq!(raw_message_local_name(message).unwrap(), "GGV-11-12-TEST");
        assert!(raw_message_local_name(b"001||01\n201||^E\n").is_err());
        assert_eq!(raw_message_local_name_value(b"009||  ").is_err(), true);
        assert_eq!(
            raw_message_optional_local_name(b"008||945\n").unwrap(),
            None
        );
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
        assert_eq!(usable_serial_start(true, 1), Some(100));
        assert_eq!(usable_serial_start(true, 784), Some(800));
        assert_eq!(usable_serial_start(true, 800), Some(800));
        assert_eq!(usable_serial_start(true, 801), Some(900));
    }

    #[test]
    fn preserves_serial_digit_count_after_rounding_to_next_hundred() {
        let rounded = usable_serial_start(true, 784).unwrap();
        let formatted = format_serial_start(rounded, Some(5)).unwrap();
        assert_eq!(formatted, "00800");

        let message = STANDARD.encode("207||C0,00000,+1,A1\n201||^C0\n");
        let printed = ezpl_from_base64_with_serial(
            &message,
            "LOT42",
            0,
            0,
            &PrintOverrides::default(),
            None,
            Some(&formatted),
        )
        .unwrap();
        assert!(printed.ezpl.contains("C0,00800,+1,A1"));
        assert!(printed.ezpl.contains("^C00800"));
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
    fn ignores_200_comment_rows_for_print_and_autosave() {
        let message = STANDARD.encode("200||debug comment\n201||^E\n");
        let processed =
            ezpl_from_base64(&message, "LOT42", 0, 0, &PrintOverrides::default()).unwrap();
        assert_eq!(processed.ezpl, "^E\n");

        let autosave =
            String::from_utf8(STANDARD.decode(processed.autosave_base64).unwrap()).unwrap();
        assert_eq!(autosave, "008||0000\n009||N/A\n201||^E\n");
        assert!(!autosave.contains("200||"));
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
            Some("12345"),
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
    fn matrix_231_or_232_shift_changes_only_xy_and_preserves_encoding_parameters() {
        let message = STANDARD.encode("232||XRB0220,0020,5,0,27\n");
        let processed =
            ezpl_from_base64(&message, "LOT42", 50, 30, &PrintOverrides::default()).unwrap();

        assert_eq!(processed.ezpl, "XRB0270,0050,5,0,27\n");

        let mislabeled_message = STANDARD.encode("231||XRB0220,0020,5,0,27\n");
        let mislabeled = ezpl_from_base64(
            &mislabeled_message,
            "LOT42",
            50,
            30,
            &PrintOverrides::default(),
        )
        .unwrap();
        assert_eq!(mislabeled.ezpl, "XRB0270,0050,5,0,27\n");
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
