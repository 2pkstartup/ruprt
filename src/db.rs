use chrono::{Datelike, Days, NaiveDate};
use mysql::{Pool, Row, params, prelude::Queryable};
use std::{collections::BTreeSet, error::Error};

const LOT_DATE_CODES: [char; 17] = [
    'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'J', 'K', 'L', 'M', 'N', 'Q', 'R', 'X', 'Z',
];

#[derive(Debug, PartialEq, Eq)]
/// The identifying columns returned by `specs.tbl_serials`.
pub struct SerialConfig {
    pub id: u64,
    pub project_id: u32,
    pub coding: u32,
    pub digit_count: Option<u32>,
}

#[derive(Debug, PartialEq, Eq)]
/// Result of checking serialization and finding the highest SN for a date.
pub struct SerialLookup {
    pub is_serial: bool,
    pub max_serial: u64,
}

/// Reads a project's serialization settings from the shared `specs` database.
/// Returns `None` when the project has no row in `specs.tbl_serials`.
pub fn find_serial_config(
    mysql_url: &str,
    project_id: u32,
) -> Result<Option<SerialConfig>, mysql::Error> {
    let pool = Pool::new(mysql_url)?;
    let mut connection = pool.get_conn()?;
    let row: Option<(u64, u32, u32, Option<u32>)> = connection.exec_first(
        "SELECT s.id, s.projectID, s.coding, c.digit_count FROM specs.tbl_serials s LEFT JOIN specs.tbl_coding c ON s.coding = c.id WHERE s.projectID = :project_id",
        params! { "project_id" => project_id },
    )?;

    Ok(
        row.map(|(id, project_id, coding, digit_count)| SerialConfig {
            id,
            project_id,
            coding,
            digit_count,
        }),
    )
}

/// Loads the latest stored message for a project and line.
/// Date breaks ties by most recent message; `id` makes the order deterministic.
pub fn latest_message(
    mysql_url: &str,
    project_id: u32,
    line: u32,
) -> Result<Option<String>, mysql::Error> {
    let pool = Pool::new(mysql_url)?;
    let mut connection = pool.get_conn()?;
    connection.exec_first(
        "SELECT mess_64 FROM mess.tbl_mess WHERE projectID = :project_id AND line = :line ORDER BY date DESC, id DESC LIMIT 1",
        params! {
            "project_id" => project_id,
            "line" => line,
        },
    )
}

/// Saves a printed file-based message into the autosave partition for this month.
pub fn save_autosave_message(
    mysql_url: &str,
    project_id: u32,
    name: &str,
    line: u32,
    printer_id: u32,
    mess_64: &str,
) -> Result<(), mysql::Error> {
    let pool = Pool::new(mysql_url)?;
    let mut connection = pool.get_conn()?;
    let current_month: Option<(i32, u32)> =
        connection.query_first("SELECT YEAR(CURRENT_DATE), MONTH(CURRENT_DATE)")?;
    let (year, month) = current_month.ok_or_else(|| {
        mysql::Error::IoError(std::io::Error::other("database returned no current month"))
    })?;
    let month_index = (year - 2000) * 12 + month as i32;
    if month_index <= 0 {
        return Err(mysql::Error::IoError(std::io::Error::other(
            "database returned an unsupported current month",
        )));
    }

    let table = format!("tbl_auto_{month_index:04}");
    let query = format!(
        "INSERT INTO `mess`.`{table}` (`date`, `projectID`, `name`, `valid`, `line`, `printer`, `archived`, `desc`, `mess_64`) VALUES (NOW(), :project_id, :name, 0, :line, :printer, 0, 'autosave', :mess_64)"
    );
    connection.exec_drop(
        query,
        params! {
            "project_id" => project_id,
            "name" => name,
            "line" => line,
            "printer" => printer_id,
            "mess_64" => mess_64,
        },
    )
}

/// Generates the printer's QR/DataMatrix payload for one project and line.
pub fn generate_serial_code(
    mysql_url: &str,
    project_id: u32,
    line: u32,
) -> Result<String, Box<dyn Error>> {
    let pool = Pool::new(mysql_url)?;
    let mut connection = pool.get_conn()?;
    connection.exec_drop(
        "CALL specs.SERNUM(@sn, :project_id, NOW(), :line)",
        params! {
            "project_id" => project_id,
            "line" => line,
        },
    )?;

    let serial_code: Option<String> = connection.query_first("SELECT @sn")?;
    serial_code.ok_or_else(|| "specs.SERNUM returned no serial code".into())
}

/// Calculates the project's lot by selecting the LOT() result matching DateCode.
/// The procedure's result columns follow `LOT_DATE_CODES` order.
pub fn calculate_project_lot(
    mysql_url: &str,
    project_id: u32,
    date: NaiveDate,
) -> Result<Option<String>, Box<dyn Error>> {
    let pool = Pool::new(mysql_url)?;
    let mut connection = pool.get_conn()?;
    let date_code: Option<String> = connection.exec_first(
        "SELECT DateCode FROM specs.tbl_valves WHERE projectID = :project_id LIMIT 1",
        params! { "project_id" => project_id },
    )?;
    let Some(date_code) = date_code else {
        return Ok(None);
    };
    let Some(code) = date_code.trim().chars().next() else {
        return Ok(None);
    };
    let Some(result_column) = LOT_DATE_CODES
        .iter()
        .position(|candidate| *candidate == code)
    else {
        return Err(format!("Unsupported DateCode prefix: {code}").into());
    };

    let date_argument = date.format("%Y-%m-%d").to_string();
    let result: Option<Row> =
        connection.exec_first("CALL specs.LOT(:date)", params! { "date" => date_argument })?;

    Ok(result.and_then(|row| row.get::<String, usize>(result_column)))
}

#[cfg(test)]
mod lot_tests {
    use super::LOT_DATE_CODES;

    #[test]
    fn date_code_d_selects_the_fourth_lot_result() {
        assert_eq!(LOT_DATE_CODES.iter().position(|code| *code == 'D'), Some(3));
    }

    #[test]
    fn maps_all_supported_date_code_prefixes_in_procedure_order() {
        assert_eq!(
            LOT_DATE_CODES,
            [
                'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'J', 'K', 'L', 'M', 'N', 'Q', 'R', 'X', 'Z'
            ]
        );
    }
}

/// Converts a project and lot code to its print date using the shared SQL function.
/// A SQL `NULL` result is returned as `None` when the lot has no date mapping.
fn parse_nullable_lot_date(
    result: Option<Option<String>>,
) -> Result<Option<NaiveDate>, chrono::ParseError> {
    match result {
        Some(Some(value)) => NaiveDate::parse_from_str(&value, "%Y-%m-%d").map(Some),
        Some(None) | None => Ok(None),
    }
}

pub fn lot_to_date(
    mysql_url: &str,
    project_id: u32,
    lot: &str,
) -> Result<Option<NaiveDate>, Box<dyn Error>> {
    let pool = Pool::new(mysql_url)?;
    let mut connection = pool.get_conn()?;
    let date: Option<Option<String>> = connection.exec_first(
        "SELECT DATE_FORMAT(specs.LOT_TO_DATE(:project_id, :lot), '%Y-%m-%d')",
        params! {
            "project_id" => project_id,
            "lot" => lot,
        },
    )?;

    parse_nullable_lot_date(date).map_err(Into::into)
}

#[cfg(test)]
mod nullable_lot_date_tests {
    use super::parse_nullable_lot_date;
    use chrono::NaiveDate;

    #[test]
    fn sql_null_means_no_matching_lot_date() {
        assert_eq!(parse_nullable_lot_date(Some(None)).unwrap(), None);
        assert_eq!(parse_nullable_lot_date(None).unwrap(), None);
    }

    #[test]
    fn parses_a_non_null_lot_date() {
        assert_eq!(
            parse_nullable_lot_date(Some(Some("2026-10-03".to_owned()))).unwrap(),
            Some(NaiveDate::from_ymd_opt(2026, 10, 3).unwrap())
        );
    }
}

/// Finds the highest serial number for a project and print date.
///
/// The stored procedure searches log records within two days on either side of
/// `date_of_print`. We first discover every line represented in those date
/// partitions, then call the procedure once per distinct line and keep the
/// greatest returned SN. A project without a `tbl_serials` row is not serial.
pub fn find_max_serial_number(
    mysql_url: &str,
    project_id: u32,
    date_of_print: NaiveDate,
) -> Result<SerialLookup, mysql::Error> {
    let pool = Pool::new(mysql_url)?;
    let mut connection = pool.get_conn()?;

    let is_serial: bool = connection
        .exec_first(
            "SELECT EXISTS(SELECT 1 FROM specs.tbl_serials WHERE projectID = :project_id)",
            params! { "project_id" => project_id },
        )?
        .unwrap_or(false);

    if !is_serial {
        return Ok(SerialLookup {
            is_serial: false,
            max_serial: 0,
        });
    }

    // The procedure uses the same inclusive date bounds for its log search.
    let start_date = date_of_print
        .checked_sub_days(Days::new(2))
        .expect("supported date has a two-day lookback");
    let end_date = date_of_print
        .checked_add_days(Days::new(2))
        .expect("supported date has a two-day lookahead");
    let start_month = month_index(start_date);
    let end_month = month_index(end_date);
    // Log databases are grouped by project hundreds; table suffix is the
    // one-based month count beginning at January 2000.
    let database = format!("logs_{:02}", (project_id + 50) / 100);
    let project_table = format!("{project_id:04}");
    let mut lines = BTreeSet::new();

    for month in start_month..=end_month {
        let table = format!("tbl_{project_table}_{month:04}");
        // Bind values for metadata checks; dynamic identifiers below are built
        // only from the numeric project ID and calculated month index.
        let table_exists: bool = connection.exec_first(
            "SELECT EXISTS(SELECT 1 FROM information_schema.tables WHERE table_schema = :database AND table_name = :table)",
            params! { "database" => &database, "table" => &table },
        )?.unwrap_or(false);

        if !table_exists {
            continue;
        }

        // SQL parameters cannot stand in for schema/table identifiers, so only
        // the validated numeric-derived names are interpolated here.
        let query = format!(
            "SELECT DISTINCT line FROM `{database}`.`{table}` WHERE date >= :start_date AND date <= :end_date AND project = :project_id"
        );
        let table_lines: Vec<u32> = connection.exec(
            query,
            params! {
                "start_date" => start_date.format("%Y-%m-%d 00:00:00").to_string(),
                "end_date" => end_date.format("%Y-%m-%d 00:00:00").to_string(),
                "project_id" => project_id,
            },
        )?;
        lines.extend(table_lines);
    }

    // The procedure accepts DATETIME; this CLI currently supplies date-only input.
    let date_argument = date_of_print.format("%Y-%m-%d 00:00:00").to_string();
    let mut max_serial = 0;
    for line in lines {
        // Each call returns the maximum SN for one line. The winner is the
        // largest result across all lines that had records in the search window.
        let serial: Option<u64> = connection.exec_first(
            "CALL specs.FIND_MAX_SERIAL_NUMB(:project_id, :date_of_print, :line)",
            params! {
                "project_id" => project_id,
                "date_of_print" => &date_argument,
                "line" => line,
            },
        )?;
        max_serial = max_serial.max(serial.unwrap_or(0));
    }

    Ok(SerialLookup {
        is_serial: true,
        max_serial,
    })
}

/// Converts a calendar date to the project's one-based month table suffix.
/// January 2000 maps to 1, February 2000 to 2, and so on.
fn month_index(date: NaiveDate) -> i32 {
    (date.year() - 2000) * 12 + date.month() as i32
}

#[cfg(test)]
mod tests {
    use super::month_index;
    use chrono::NaiveDate;

    #[test]
    fn computes_month_index_from_january_2000() {
        assert_eq!(month_index(NaiveDate::from_ymd_opt(2000, 1, 1).unwrap()), 1);
        assert_eq!(
            month_index(NaiveDate::from_ymd_opt(2026, 10, 3).unwrap()),
            322
        );
    }
}
