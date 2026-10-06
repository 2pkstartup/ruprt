use chrono::{Datelike, Days, NaiveDate};
use mysql::{Pool, params, prelude::Queryable};
use std::{collections::BTreeSet, error::Error};

#[derive(Debug, PartialEq, Eq)]
/// The identifying columns returned by `specs.tbl_serials`.
pub struct SerialConfig {
    pub id: u64,
    pub project_id: u32,
    pub coding: u32,
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
    let row: Option<(u64, u32, u32)> = connection.exec_first(
        "SELECT id, projectID, coding FROM specs.tbl_serials WHERE projectID = :project_id",
        params! { "project_id" => project_id },
    )?;

    Ok(row.map(|(id, project_id, coding)| SerialConfig {
        id,
        project_id,
        coding,
    }))
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

/// Converts a project and lot code to its print date using the shared SQL function.
/// A SQL `NULL` result is returned as `None` when the lot has no date mapping.
pub fn lot_to_date(
    mysql_url: &str,
    project_id: u32,
    lot: &str,
) -> Result<Option<NaiveDate>, Box<dyn Error>> {
    let pool = Pool::new(mysql_url)?;
    let mut connection = pool.get_conn()?;
    let date: Option<String> = connection.exec_first(
        "SELECT DATE_FORMAT(specs.LOT_TO_DATE(:project_id, :lot), '%Y-%m-%d')",
        params! {
            "project_id" => project_id,
            "lot" => lot,
        },
    )?;

    date.map(|value| NaiveDate::parse_from_str(&value, "%Y-%m-%d"))
        .transpose()
        .map_err(Into::into)
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
