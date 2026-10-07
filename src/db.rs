//! MySQL accessors shared by the CLI applications.
//!
//! Values passed into SQL statements are bound parameters. Dynamic log table
//! identifiers are derived only from validated project/date numbers.

// =============================================================================
// Komentovaná verze – kód je původní, přibyly jen české komentáře (// ...).
// Původní anglické komentáře (///, //!, //) jsou ponechány beze změny.
//
// Tento soubor je KNIHOVNÍ MODUL (bez main) s funkcemi pro práci s MySQL. Používají ho
// programy rusn a rusend. Funkce v souboru:
//   find_serial_config      – nastavení sériových čísel projektu (tabulky tbl_serials, tbl_coding)
//   latest_message          – poslední uložená zpráva pro projekt a linku
//   save_autosave_message   – uloží vytištěnou zprávu do měsíční tabulky tbl_auto_NNNN
//   generate_serial_code    – zavolá proceduru specs.SERNUM a vrátí kód pro tiskárnu
//   calculate_project_lot   – spočítá lot z data (procedura specs.LOT)
//   lot_to_date             – opačný směr: z lotu datum (SQL funkce specs.LOT_TO_DATE)
//   find_max_serial_number  – najde nejvyšší sériové číslo (volá specs.FIND_MAX_SERIAL_NUMB)
//
// Dvě bezpečnostní zásady, které soubor dodržuje:
//   1) HODNOTY se do SQL předávají jako vázané parametry (:project_id, :lot ...) přes params!,
//      takže se nikdy nelepí do textu dotazu (ochrana proti SQL injection).
//   2) NÁZVY tabulek a databází nejdou předat parametrem, proto se skládají přes format!,
//      ale jen z čísel (ID projektu, index měsíce).
//
// Poznámka: každá funkce si vytváří vlastní Pool a spojení. U krátce žijícího CLI nástroje
// to nevadí; u dlouho běžící aplikace by se pool vytvořil jednou a sdílel.
// =============================================================================

// Co se importuje:
//   chrono::NaiveDate = datum bez časové zóny; Datelike = trait s metodami .year(), .month();
//   chrono::Days      = počet dnů pro počítání s daty (data +/- N dní);
//   mysql::Pool       = zásobník spojení do databáze; Row = jeden řádek výsledku;
//   mysql::params     = makro params! pro pojmenované parametry dotazů;
//   Queryable         = trait s metodami .exec_first(), .query_first(), .exec(), .exec_drop()...;
//   BTreeSet          = množina bez duplicit, kterou drží prvky seřazené;
//   Error             = společný trait chyb (pro Box<dyn Error>).
use chrono::{Datelike, Days, NaiveDate};
use mysql::{Pool, Row, params, prelude::Queryable};
use std::{collections::BTreeSet, error::Error};

// `const` = konstanta známá při překladu, jméno se píše VELKÝMI_PÍSMENY.
// Typ [char; 17] = pole přesně 17 znaků (délka je součástí typu).
// Pořadí písmen odpovídá pořadí sloupců, které vrací SQL procedura LOT.
const LOT_DATE_CODES: [char; 17] = [
    'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'J', 'K', 'L', 'M', 'N', 'Q', 'R', 'X', 'Z',
];

// derive: Debug (výpis přes {:?}), PartialEq a Eq (porovnávání pomocí ==).
#[derive(Debug, PartialEq, Eq)]
/// The identifying columns returned by `specs.tbl_serials`.
// `pub` u struktury i u každého pole = viditelné i mimo tento soubor.
// Option<u32> = hodnota nemusí existovat (None); tady proto, že LEFT JOIN může vrátit NULL.
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
    pub digit_count: Option<u32>,
}

/// Reads a project's serialization settings from the shared `specs` database.
/// Returns `None` when the project has no row in `specs.tbl_serials`.
// `mysql_url: &str` = půjčený text (funkce si ho nebere do vlastnictví).
// Návratový typ má tři možné výsledky:
//   Err(chyba)       – selhalo spojení nebo dotaz,
//   Ok(None)         – dotaz proběhl, ale projekt v tabulce není,
//   Ok(Some(config)) – projekt nalezen.
pub fn find_serial_config(
    mysql_url: &str,
    project_id: u32,
) -> Result<Option<SerialConfig>, mysql::Error> {
    // Pool::new vytvoří zásobník spojení z URL; get_conn() z něj vezme jedno spojení.
    // `?` = při chybě funkci hned ukonči a chybu vrať volajícímu.
    // `mut`, protože provádění dotazů mění stav spojení.
    let pool = Pool::new(mysql_url)?;
    let mut connection = pool.get_conn()?;
    // exec_first provede dotaz s parametry a vrátí PRVNÍ řádek (nebo None, když žádný není).
    // Typ za dvojtečkou říká, jak se řádek převede: dvojice sloupců na čtveřici (tuple).
    // Poslední položka je Option<u32>, protože LEFT JOIN může dát NULL.
    // :project_id v SQL je pojmenovaný parametr; params! mu přiřadí hodnotu.
    let row: Option<(u64, u32, u32, Option<u32>)> = connection.exec_first(
        "SELECT s.id, s.projectID, s.coding, c.digit_count FROM specs.tbl_serials s LEFT JOIN specs.tbl_coding c ON s.coding = c.id WHERE s.projectID = :project_id",
        params! { "project_id" => project_id },
    )?;

    // Option::map změní hodnotu uvnitř Some a None nechá být. Closure |(id, ...)| rovnou
    // rozebere čtveřici na proměnné a vyrobí z nich strukturu. Zápis `id,` je zkratka
    // za `id: id,` (proměnná se jmenuje stejně jako pole).
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
    // Poslední výraz funkce je bez středníku a bez `?`, takže se Result vrací rovnou volajícímu.
    // Typ výsledku (Option<String>) si Rust odvodí z návratového typu funkce.
    // ORDER BY date DESC, id DESC LIMIT 1 = nejnovější záznam (při shodě data vyhraje vyšší id).
    connection.exec_first(
        "SELECT mess_64 FROM mess.tbl_mess WHERE projectID = :project_id AND line = :line ORDER BY date DESC, id DESC LIMIT 1",
        params! {
            "project_id" => project_id,
            "line" => line,
        },
    )
}

/// Saves a printed file-based message into the autosave partition for this month.
// Result<(), mysql::Error>: `()` je "prázdná hodnota" – funkce při úspěchu nic nevrací,
// jen oznámí, že proběhla (Ok(())).
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
    // Partition selection uses the DB server's clock, matching its NOW() insert timestamp.
    // query_first = dotaz bez parametrů, vrátí první řádek (zde dvojici rok, měsíc).
    let current_month: Option<(i32, u32)> =
        connection.query_first("SELECT YEAR(CURRENT_DATE), MONTH(CURRENT_DATE)")?;
    // .ok_or_else(...) změní Option na Result: Some(x) -> Ok(x), None -> Err(naše chyba).
    // `?` pak při chybě funkci ukončí. `let (year, month) = ...` rozebere dvojici na dvě proměnné.
    // std::io::Error::other(text) vytvoří obecnou I/O chybu s popisem; mysql::Error::IoError
    // ji zabalí do chybového typu této knihovny.
    let (year, month) = current_month.ok_or_else(|| {
        mysql::Error::IoError(std::io::Error::other("database returned no current month"))
    })?;
    // Index měsíce = počet měsíců od ledna 2000 (leden 2000 = 1). `month as i32` je přetypování
    // u32 na i32 (aby šlo sčítat s `year`); `as` má přednost před `+`.
    let month_index = (year - 2000) * 12 + month as i32;
    if month_index <= 0 {
        return Err(mysql::Error::IoError(std::io::Error::other(
            "database returned an unsupported current month",
        )));
    }

    // format! skládá text. {month_index:04} = číslo doplněné nulami na 4 místa (322 -> "0322").
    // Název tabulky nejde předat jako parametr dotazu, proto se vkládá přímo do textu
    // (je ale složen jen z čísla, které jsme spočítali).
    let table = format!("tbl_auto_{month_index:04}");
    let query = format!(
        "INSERT INTO `mess`.`{table}` (`date`, `projectID`, `name`, `valid`, `line`, `printer`, `archived`, `desc`, `mess_64`) VALUES (NOW(), :project_id, :name, 0, :line, :printer, 0, 'autosave', :mess_64)"
    );
    // exec_drop provede dotaz a případný výsledek zahodí (INSERT žádná data nevrací).
    // Jako poslední výraz funkce vrací rovnou Result<(), mysql::Error>.
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
// Box<dyn Error> = "krabice" na libovolný druh chyby; díky tomu může `?` v jedné funkci
// vracet různé chybové typy.
pub fn generate_serial_code(
    mysql_url: &str,
    project_id: u32,
    line: u32,
) -> Result<String, Box<dyn Error>> {
    let pool = Pool::new(mysql_url)?;
    let mut connection = pool.get_conn()?;
    // The session variable must be read on this same connection after the procedure call.
    // @sn je uživatelská proměnná MySQL, která žije jen v rámci jednoho spojení. Procedura do ní
    // zapíše výsledek, proto se čte ve stejném spojení (`connection`) a ne v novém.
    connection.exec_drop(
        "CALL specs.SERNUM(@sn, :project_id, NOW(), :line)",
        params! {
            "project_id" => project_id,
            "line" => line,
        },
    )?;

    // Přečtení hodnoty proměnné. Je typu Option<String>: může být NULL.
    // .ok_or_else(|| "...".into()) – při None vyrobí chybu; `.into()` převede text na Box<dyn Error>.
    // Řádek bez středníku je návratová hodnota funkce.
    let serial_code: Option<String> = connection.query_first("SELECT @sn")?;
    serial_code.ok_or_else(|| "specs.SERNUM returned no serial code".into())
}

/// Calculates the project's lot by selecting the LOT() result matching DateCode.
/// The procedure's result columns follow `LOT_DATE_CODES` order.
// `date: NaiveDate` = datum bez času a časové zóny (typ z knihovny chrono).
pub fn calculate_project_lot(
    mysql_url: &str,
    project_id: u32,
    date: NaiveDate,
) -> Result<Option<String>, Box<dyn Error>> {
    let pool = Pool::new(mysql_url)?;
    let mut connection = pool.get_conn()?;
    // Zjistí typ kódu data (písmeno) pro projekt. LIMIT 1 = jen první řádek.
    let date_code: Option<String> = connection.exec_first(
        "SELECT DateCode FROM specs.tbl_valves WHERE projectID = :project_id LIMIT 1",
        params! { "project_id" => project_id },
    )?;
    // `let Some(x) = hodnota else { ... };` ("let-else"): když hodnota odpovídá vzoru Some(x),
    // proměnná x je k dispozici dál; jinak se provede blok else, který musí funkci opustit
    // (tady return). Jméno date_code se použije znovu = "shadowing" (nová proměnná zastíní starou).
    let Some(date_code) = date_code else {
        return Ok(None);
    };
    // .trim() ořízne mezery, .chars() projde znaky, .next() vezme první (Option<char>;
    // u prázdného textu None).
    let Some(code) = date_code.trim().chars().next() else {
        return Ok(None);
    };
    // LOT returns one column per supported DateCode family in the constant's order.
    // .iter().position(...) vrátí index prvního prvku, pro který closure platí (Option<usize>).
    // `*candidate == code`: candidate je odkaz na znak, hvězdička ho "rozbalí" k porovnání.
    // Index písmene = číslo sloupce ve výsledku procedury. Neznámé písmeno je chyba.
    let Some(result_column) = LOT_DATE_CODES
        .iter()
        .position(|candidate| *candidate == code)
    else {
        return Err(format!("Unsupported DateCode prefix: {code}").into());
    };

    // Datum se převede na text ve formátu RRRR-MM-DD.
    let date_argument = date.format("%Y-%m-%d").to_string();
    // Row = obecný řádek výsledku (sloupce se vybírají podle indexu, ne předem daným typem).
    let result: Option<Row> =
        connection.exec_first("CALL specs.LOT(:date)", params! { "date" => date_argument })?;

    // .and_then(...) = pokud je výsledek Some(row), zavolej closure, která vrací také Option;
    // None zůstane None. row.get::<String, usize>(i) vezme sloupec číslo i jako String
    // (turbofish ::<String, usize> říká, jaký typ hodnoty a jaký typ indexu).
    Ok(result.and_then(|row| row.get::<String, usize>(result_column)))
}

// Soubor obsahuje víc testovacích modulů (lot_tests, nullable_lot_date_tests, tests) – je to v pořádku.
// #[cfg(test)] = modul se překládá jen při `cargo test`.
#[cfg(test)]
mod lot_tests {
    // `super` = nadřazený modul (tento soubor). Podmodul vidí i soukromé položky rodiče.
    use super::LOT_DATE_CODES;

    // #[test] označí testovací funkci; projde, pokud nezpanikaří.
    // Písmeno 'D' musí být na indexu 3 (počítá se od 0, tedy čtvrté).
    #[test]
    fn date_code_d_selects_the_fourth_lot_result() {
        assert_eq!(LOT_DATE_CODES.iter().position(|code| *code == 'D'), Some(3));
    }

    // Hlídá, aby se pořadí písmen v konstantě omylem nezměnilo (musí sedět na sloupce procedury).
    // Pole stejné délky a typu lze porovnat přímo operátorem ==.
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
// Pozn.: dva /// řádky výše popisují spíš funkci lot_to_date níže; tady je pomocná funkce,
// která zpracuje výsledek dotazu na datum. Bez `pub` je viditelná jen v tomto souboru.
// Vstup Option<Option<String>> má dvě vrstvy:
//   vnější Option  – existoval řádek výsledku? (None = žádný řádek),
//   vnitřní Option – byla hodnota v řádku NULL? (None = SQL NULL).
// Chyba chrono::ParseError znamená, že text nejde přečíst jako datum.
fn parse_nullable_lot_date(
    result: Option<Option<String>>,
) -> Result<Option<NaiveDate>, chrono::ParseError> {
    // `match` rozliší případy. `|` v jedné větvi znamená "nebo": Some(None) i None dávají Ok(None).
    match result {
        // Řádek existuje a hodnota není NULL: text se převede na datum. Parse vrací
        // Result<NaiveDate, _>; .map(Some) zabalí datum uvnitř Ok do Some
        // (název `Some` se tu předává jako funkce).
        Some(Some(value)) => NaiveDate::parse_from_str(&value, "%Y-%m-%d").map(Some),
        Some(None) | None => Ok(None),
    }
}

// Veřejná funkce: lot -> datum přes SQL funkci specs.LOT_TO_DATE (ta se stará o všechny formáty kódů).
pub fn lot_to_date(
    mysql_url: &str,
    project_id: u32,
    lot: &str,
) -> Result<Option<NaiveDate>, Box<dyn Error>> {
    let pool = Pool::new(mysql_url)?;
    let mut connection = pool.get_conn()?;
    // DATE_FORMAT dá text RRRR-MM-DD; z NULL vznikne zase NULL. Proto Option<Option<String>>.
    // Hodnoty :project_id a :lot jdou jako vázané parametry, ne slepené do textu.
    let date: Option<Option<String>> = connection.exec_first(
        "SELECT DATE_FORMAT(specs.LOT_TO_DATE(:project_id, :lot), '%Y-%m-%d')",
        params! {
            "project_id" => project_id,
            "lot" => lot,
        },
    )?;

    // .map_err(Into::into) převede chybu ParseError na Box<dyn Error>, aby seděla do návratového typu.
    // `Into::into` je předání funkce místo zápisu closure |e| e.into().
    parse_nullable_lot_date(date).map_err(Into::into)
}

#[cfg(test)]
mod nullable_lot_date_tests {
    use super::parse_nullable_lot_date;
    use chrono::NaiveDate;

    // SQL NULL (Some(None)) i chybějící řádek (None) znamenají "datum neznámé" = Ok(None).
    // .unwrap() vybalí hodnotu z Ok (u Err test zpanikaří).
    #[test]
    fn sql_null_means_no_matching_lot_date() {
        assert_eq!(parse_nullable_lot_date(Some(None)).unwrap(), None);
        assert_eq!(parse_nullable_lot_date(None).unwrap(), None);
    }

    // NaiveDate::from_ymd_opt(rok, měsíc, den) vrací Option (None u neplatného data),
    // proto za ním následuje .unwrap(). Text "2026-10-03" musí vyjít jako 3. října 2026.
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

    // SELECT EXISTS(...) vrací 0 nebo 1, což se převede na bool. exec_first dává Option<bool>;
    // `?` vyřeší chybu a .unwrap_or(false) nahradí případné None hodnotou false.
    let is_serial: bool = connection
        .exec_first(
            "SELECT EXISTS(SELECT 1 FROM specs.tbl_serials WHERE projectID = :project_id)",
            params! { "project_id" => project_id },
        )?
        .unwrap_or(false);

    // Projekt bez sériových čísel: vrátí se hned (předčasný `return`) s nulou a bez šířky.
    if !is_serial {
        return Ok(SerialLookup {
            is_serial: false,
            max_serial: 0,
            digit_count: None,
        });
    }

    // Dvě vrstvy Option: vnější = existuje řádek, vnitřní = digit_count není NULL (LEFT JOIN).
    // Později se slučují přes .flatten().
    let digit_count: Option<Option<u32>> = connection.exec_first(
        "SELECT c.digit_count FROM specs.tbl_serials s LEFT JOIN specs.tbl_coding c ON s.coding = c.id WHERE s.projectID = :project_id",
        params! { "project_id" => project_id },
    )?;

    // The procedure uses the same inclusive date bounds for its log search.
    // checked_sub_days / checked_add_days vrací Option (None, kdyby datum přetekl rozsah);
    // .expect("...") hodnotu vybalí, nebo při None program ukončí (panic) se zadanou hláškou.
    // Days::new(2) = 2 dny.
    let start_date = date_of_print
        .checked_sub_days(Days::new(2))
        .expect("supported date has a two-day lookback");
    let end_date = date_of_print
        .checked_add_days(Days::new(2))
        .expect("supported date has a two-day lookahead");
    // Funkce month_index je definovaná níže v souboru (pořadí funkcí v Rustu nehraje roli).
    let start_month = month_index(start_date);
    let end_month = month_index(end_date);
    // Log databases are grouped by project hundreds; table suffix is the
    // one-based month count beginning at January 2000.
    // {:02} = číslo doplněné nulami na 2 místa. Celočíselné dělení (project_id + 50) / 100
    // zaokrouhlí ID na nejbližší stovku (149 -> 1, 150 -> 2), tedy logs_01, logs_02, ...
    // {project_id:04} = ID doplněné nulami na 4 místa (např. 0628).
    let database = format!("logs_{:02}", (project_id + 50) / 100);
    let project_table = format!("{project_id:04}");
    // BTreeSet = množina čísel linek: bez duplicit a seřazená vzestupně.
    // Typ prvků (u32) se odvodí z toho, co do ní přidáme níže.
    let mut lines = BTreeSet::new();

    // The procedure spans at most adjacent months; inspect each corresponding partition.
    // `a..=b` je rozsah včetně obou mezí (start_month až end_month); běžně to je jeden měsíc,
    // u data na přelomu měsíců dva.
    for month in start_month..=end_month {
        let table = format!("tbl_{project_table}_{month:04}");
        // Bind values for metadata checks; dynamic identifiers below are built
        // only from the numeric project ID and calculated month index.
        // Kontrola, zda tabulka v databázi existuje. `&database` a `&table` jsou odkazy
        // (půjčení), aby se proměnné nepřesunuly a šly použít i dál.
        let table_exists: bool = connection.exec_first(
            "SELECT EXISTS(SELECT 1 FROM information_schema.tables WHERE table_schema = :database AND table_name = :table)",
            params! { "database" => &database, "table" => &table },
        )?.unwrap_or(false);

        // Tabulka neexistuje -> přeskoč zbytek těla cyklu a pokračuj dalším měsícem.
        if !table_exists {
            continue;
        }

        // SQL parameters cannot stand in for schema/table identifiers, so only
        // the validated numeric-derived names are interpolated here.
        // Dotaz skládaný z názvů databáze a tabulky ({database}, {table} se doplní z proměnných).
        // Backticky ` ` v SQL ohraničují identifikátory. Hodnoty (:start_date ...) zůstávají parametry.
        let query = format!(
            "SELECT DISTINCT line FROM `{database}`.`{table}` WHERE date >= :start_date AND date <= :end_date AND project = :project_id"
        );
        // .exec (na rozdíl od exec_first) vrátí VŠECHNY řádky. Typ Vec<u32> říká, že každý řádek
        // je jedno číslo (sloupec line). Data se předávají jako text "RRRR-MM-DD 00:00:00".
        let table_lines: Vec<u32> = connection.exec(
            query,
            params! {
                "start_date" => start_date.format("%Y-%m-%d 00:00:00").to_string(),
                "end_date" => end_date.format("%Y-%m-%d 00:00:00").to_string(),
                "project_id" => project_id,
            },
        )?;
        // .extend přidá všechna čísla do množiny; duplicity (stejná linka v obou měsících) zmizí.
        lines.extend(table_lines);
    }

    // The procedure accepts DATETIME; this CLI currently supplies date-only input.
    let date_argument = date_of_print.format("%Y-%m-%d 00:00:00").to_string();
    // Číselný typ proměnné se odvodí z použití níže (u64). `mut`, protože se bude zvyšovat.
    let mut max_serial = 0;
    // Each line may have a different latest serial; select the largest procedure result.
    // `for line in lines` projde množinu vzestupně a spotřebuje ji (po cyklu už nejde použít).
    for line in lines {
        // Each call returns the maximum SN for one line. The winner is the
        // largest result across all lines that had records in the search window.
        // Procedura vrací Option<u64>: None, když nic nenašla. `&date_argument` je půjčený odkaz,
        // díky čemuž se stejný text dá použít v každém průchodu cyklu.
        let serial: Option<u64> = connection.exec_first(
            "CALL specs.FIND_MAX_SERIAL_NUMB(:project_id, :date_of_print, :line)",
            params! {
                "project_id" => project_id,
                "date_of_print" => &date_argument,
                "line" => line,
            },
        )?;
        // .max(b) vrátí větší ze dvou čísel; None se bere jako 0 (.unwrap_or(0)).
        max_serial = max_serial.max(serial.unwrap_or(0));
    }

    // Výsledek. `max_serial,` je zkratka za `max_serial: max_serial,`.
    // .flatten() spojí Option<Option<u32>> do jednoho Option<u32> (Some(Some(x)) -> Some(x),
    // Some(None) a None -> None).
    Ok(SerialLookup {
        is_serial: true,
        max_serial,
        digit_count: digit_count.flatten(),
    })
}

/// Converts a calendar date to the project's one-based month table suffix.
/// January 2000 maps to 1, February 2000 to 2, and so on.
// date.year() a date.month() jsou metody z traitu Datelike (proto je na začátku importovaný).
// Rok je i32, měsíc u32, který se musí přetypovat `as i32`, aby šlo počítat dohromady.
// Poslední výraz bez středníku = návratová hodnota. Příklad: říjen 2026 -> 26 * 12 + 10 = 322.
fn month_index(date: NaiveDate) -> i32 {
    (date.year() - 2000) * 12 + date.month() as i32
}

#[cfg(test)]
mod tests {
    use super::month_index;
    use chrono::NaiveDate;

    // Leden 2000 musí dát 1 a 3. října 2026 musí dát 322 (rok 26 * 12 měsíců + 10).
    #[test]
    fn computes_month_index_from_january_2000() {
        assert_eq!(month_index(NaiveDate::from_ymd_opt(2000, 1, 1).unwrap()), 1);
        assert_eq!(
            month_index(NaiveDate::from_ymd_opt(2026, 10, 3).unwrap()),
            322
        );
    }
}
