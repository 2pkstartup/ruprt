// =============================================================================
// Komentovaná verze – kód je původní, přibyly jen české komentáře (// ...).
// Původní anglické komentáře (/// a //) jsou ponechány beze změny.
//
// Program je CLI nástroj: dostane ID projektu a lot, zjistí z lotu datum,
// podle data najde v databázi nejvyšší sériové číslo a vypíše ho (doplněné nulami).
//
// Tipy pro čtení Rustu:
//  - Pořadí funkcí v souboru nehraje roli, program začíná ve funkci main().
//  - Poslední výraz funkce BEZ středníku je její návratová hodnota.
//  - Result<T, E> = buď Ok(hodnota), nebo Err(chyba).  Option<T> = buď Some(hodnota), nebo None.
// =============================================================================

// `use` zpřístupní jména, abychom je mohli psát krátce. `ruprt` je knihovní část
// tohoto balíčku (název z Cargo.toml). Složené závorky { } importují víc věcí najednou.
use ruprt::{
    // struktura s načtenou konfigurací (z config.toml)
    config::AppConfig,
    // funkce, které komunikují s MySQL
    db::{find_max_serial_number, find_serial_config, lot_to_date},
    // vypíše stručnou nápovědu
    print_long_help,
    print_minimal_help,
};
// Modul std::process obsahuje process::exit() pro ukončení programu s návratovým kódem.
use std::process;

// Atribut #[derive(...)] řekne kompilátoru, ať za nás vygeneruje kód pro tři "traity"
// (trait = vlastnost/rozhraní, které typ umí):
//   Debug     -> hodnotu lze vypsat přes {:?} (ladění, výpis při selhání testu)
//   PartialEq -> dvě hodnoty lze porovnat operátorem == (shodují se všechna pole)
//   Eq        -> rovnost je "úplná" (hodnota se vždy rovná sama sobě); u u32 a String platí
// Debug a PartialEq jsou tu hlavně kvůli assert_eq! v testech na konci souboru.
#[derive(Debug, PartialEq, Eq)]
/// Validated command-line inputs for looking up one project's SN.
// `struct` = vlastní datový typ složený z pojmenovaných polí.
// Bez `pub` je viditelný jen v tomto souboru (modulu).
struct Arguments {
    // u32 = celé číslo bez znaménka, 32 bitů. Záporné číslo se do něj nevejde.
    project_id: u32,
    // String = textový řetězec, který struktura VLASTNÍ (je na haldě a může růst).
    // Opakem je &str = jen "půjčený" pohled na cizí text.
    lot: String,
}

/// Pads a positive SN to the project's coding width; 0 remains the no-result sentinel.
// Parametry: serial_number je u64 (velké číslo bez znaménka),
// digit_count je Option<u32> – hodnota může chybět (None), třeba když projekt nemá šířku nastavenu.
// Vrací Result<String, String>: Ok(hotový text) nebo Err(popis chyby jako text).
fn format_serial_number(serial_number: u64, digit_count: Option<u32>) -> Result<String, String> {
    // Číslo 0 znamená "nic nenalezeno". Vrací se hned (`return` = předčasný konec funkce).
    // "0".to_owned() změní literál &str na vlastněný String.
    if serial_number == 0 {
        return Ok("0".to_owned());
    }

    // Tento příkaz se čte po krocích:
    //  1) .filter(|count| *count > 0)  – Some(x) zůstane jen když x > 0, jinak se stane None.
    //     |count| je "closure" (anonymní funkce); count je tu odkaz (&u32), proto * (dereference).
    //  2) .ok_or_else(|| ...)          – převede Option na Result: Some(x) -> Ok(x),
    //     None -> Err(text z closure). "else" znamená, že se text vytvoří jen v případě chyby.
    //  3) ?                            – operátor chyby: u Err funkce okamžitě skončí a chybu vrátí,
    //     u Ok vybalí hodnotu a pokračuje.
    //  4) as usize                     – přetypování u32 na usize (velikost závislá na platformě),
    //     protože šířku formátování a délku textu Rust vyžaduje jako usize.
    // `let digit_count` znovu používá stejné jméno = "shadowing": nová proměnná zastíní
    // původní (typ se změnil z Option<u32> na usize).
    let digit_count = digit_count
        .filter(|count| *count > 0)
        .ok_or_else(|| "project has no valid serial digit_count".to_owned())?
        as usize;
    // Číslo převedeme na text, abychom mohli zjistit, kolik má číslic.
    let serial_text = serial_number.to_string();
    // Pokud má číslo víc číslic, než je nastavená šířka, nejde ho správně doplnit -> chyba.
    if serial_text.len() > digit_count {
        // format! skládá String; {serial_number} a {digit_count} se doplní přímo z proměnných.
        return Err(format!(
            "serial number {serial_number} exceeds configured width of {digit_count} digits"
        ));
    }

    // {serial_number:0digit_count$} = vypiš serial_number, vyplň zleva nulami (0)
    // do šířky, kterou udává proměnná digit_count ($). Např. 8960 a šířka 6 -> "008960".
    // Řádek je bez středníku, takže je to návratová hodnota funkce: Ok(text).
    Ok(format!("{serial_number:0digit_count$}"))
}

/// Accepts exactly a project ID and its lot code.
/// Parses exactly the project ID and lot pair accepted by this small CLI.
// `args: &[String]` = "slice" (pohled na posloupnost Stringů). `&` znamená, že hodnoty
// jen půjčujeme (borrow), funkce je nevlastní a po skončení zůstanou volajícímu.
fn parse_args(args: &[String]) -> Result<Arguments, String> {
    // Čekáme přesně dva argumenty: ID projektu a lot.
    if args.len() != 2 {
        return Err("Usage: rusn <project_ID> <lot>".to_owned());
    }

    // args[0] je první argument (text). .parse::<u32>() se pokusí text převést na číslo
    // (zápis ::<u32> říká, na jaký typ). Vrací Result<u32, chyba_parsování>.
    // .map_err(|_| ...) vymění původní chybu za náš text; `_` = původní chybu zahazujeme.
    // `?` při chybě funkci ukončí, jinak vybalí číslo. Záporná čísla (např. "-1") parse odmítne,
    // protože u32 nemá znaménko.
    let project_id = args[0]
        .parse::<u32>()
        .map_err(|_| "project_ID must be an integer from 0 to 5000".to_owned())?;
    // Horní mez 5000 kontrolujeme ručně (typ u32 by dovolil mnohem víc).
    if project_id > 5000 {
        return Err("project_ID must be an integer from 0 to 5000".to_owned());
    }

    // .trim() ořízne mezery na okrajích. Výsledek je &str (půjčený pohled na text v args[1]).
    let lot = args[1].trim();
    if lot.is_empty() {
        return Err("lot must not be empty".to_owned());
    }

    // Vytvoření struktury. Zápis `project_id,` je zkratka za `project_id: project_id,`
    // (použije se, když se proměnná jmenuje stejně jako pole).
    // lot.to_owned() udělá z půjčeného &str vlastněný String, který může struktura držet.
    Ok(Arguments {
        project_id,
        lot: lot.to_owned(),
    })
}

/// Checks serialization, resolves the lot date, and prints only the numeric result.
// main() je vstupní bod programu. Návratové kódy procesu v tomto programu:
//   0 = úspěch (včetně "nenalezeno": -1 / 0 se vypíše normálně),
//   1 = chyba za běhu (konfigurace, databáze, formátování),
//   2 = špatné argumenty příkazové řádky.
fn main() {
    // std::env::args() je iterátor přes argumenty programu; první je název programu samotného,
    // proto .skip(1). .collect() je poskládá do kolekce. Typ `Vec<String>` (vektor = rostoucí
    // pole) je uveden u proměnné a říká collect(), co má vytvořit.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() == 1 && matches!(args[0].as_str(), "--help" | "-h") {
        print_long_help(
            "rusn",
            env!("CARGO_PKG_VERSION"),
            "Resolves a project lot to its date and prints the latest used serial number.",
            "rusn <project_ID> <lot>",
            &[
                ("project_ID", "Project number, integer from 0 to 5000."),
                ("lot", "Project lot code resolved with specs.LOT_TO_DATE."),
                ("--help, -h", "Show this help."),
            ],
            &[
                "rusn 945 XK03",
                "cargo run --release --bin rusn -- 945 XK03",
            ],
            "config.toml: mysql_url = \"mysql://USER:PASSWORD@HOST:3306/\"",
        );
        return;
    }
    // Bez argumentů se vypíše nápověda a program skončí.
    if args.is_empty() {
        print_minimal_help(
            "rusn",
            // env!(...) se vyhodnotí při překladu: verze z Cargo.toml (např. "0.1.0").
            env!("CARGO_PKG_VERSION"),
            "Finds the highest serial number for a project and lot.",
            "rusn <project_ID> <lot>",
            // \" uvnitř textu je zapsaná uvozovka.
            "config.toml: mysql_url = \"mysql://USER:PASSWORD@HOST:3306/\"",
        );
        // `return;` ve funkci main ukončí program (s kódem 0).
        return;
    }

    // `match` rozebere Result na jednotlivé případy; každá větev vrací hodnotu
    // a ta se uloží do `parsed`. &args půjčuje vektor jako slice (&Vec<String> -> &[String]).
    // process::exit(2) má speciální typ `!` ("nikdy se nevrátí"), proto ho kompilátor
    // dovolí v jedné větvi vedle větve, která vrací Arguments.
    // Chybový text se zahazuje (Err(_)) – program při špatných argumentech nic nevypíše.
    let parsed = match parse_args(&args) {
        Ok(parsed) => parsed,
        Err(_) => process::exit(2),
    };

    // Načtení konfigurace. {error} ve výpisu se doplní přímo z proměnné.
    // eprintln! píše na chybový výstup (stderr), println! na standardní (stdout).
    let config = match AppConfig::load() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("Failed to load config.toml: {error}");
            process::exit(1);
        }
    };

    // find_serial_config vrací Result<Option<...>, chyba> – dvě vrstvy:
    //   Err(...)        -> selhal dotaz do databáze,
    //   Ok(None)        -> dotaz proběhl, ale projekt v databázi není,
    //   Ok(Some(config))-> projekt nalezen, uvnitř je jeho nastavení.
    // &config.mysql_url je půjčený odkaz na pole `mysql_url` (funkce si text nebere do vlastnictví).
    // Pozor: jméno `config` ve větvi Some(config) je nová proměnná, která zastíní vnější `config`
    // jen uvnitř té větve; výsledek se uloží do `serial_config`.
    let serial_config = match find_serial_config(&config.mysql_url, parsed.project_id) {
        Ok(Some(config)) => config,
        // Projekt neexistuje: vypíše se "-1" a program normálně skončí.
        Ok(None) => {
            println!("-1");
            return;
        }
        Err(error) => {
            eprintln!("Database lookup failed: {error}");
            process::exit(1);
        }
    };

    // Z lotu se (pravděpodobně přes SQL funkci) spočítá datum tisku.
    // &parsed.lot půjčuje text lotu ze struktury. Ok(None) = z lotu se datum určit nepodařilo.
    let date_of_print = match lot_to_date(&config.mysql_url, parsed.project_id, &parsed.lot) {
        Ok(Some(date)) => date,
        // Datum nelze určit: vypíše se "0" (stejná hodnota jako "nic nenalezeno").
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
    // Zavolá se vyhledání nejvyššího sériového čísla pro projekt a datum.
    // Výsledek (`lookup`) je struktura, z níž dál používáme pole max_serial.
    let lookup = match find_max_serial_number(&config.mysql_url, parsed.project_id, date_of_print) {
        Ok(result) => result,
        Err(error) => {
            eprintln!("Database lookup failed: {error}");
            process::exit(1);
        }
    };

    // Doplnění nulami na šířku podle nastavení projektu (viz format_serial_number nahoře).
    let serial_output = match format_serial_number(lookup.max_serial, serial_config.digit_count) {
        Ok(serial) => serial,
        Err(error) => {
            eprintln!("Unable to format serial number: {error}");
            process::exit(1);
        }
    };
    // Jediný řádek, který program při úspěchu vypíše na stdout – jen číslo.
    println!("{serial_output}");
}

// #[cfg(test)] znamená "tento blok přelož jen při `cargo test`". Do běžné binárky se nedostane.
// `mod` vytváří modul (jmenný prostor); testy se tradičně dávají do modulu `tests`.
#[cfg(test)]
mod tests {
    // `super` = nadřazený modul (tedy tento soubor nahoře). Podmodul vidí i soukromé
    // položky rodiče, proto lze importovat i funkce a strukturu bez `pub`.
    use super::{Arguments, format_serial_number, parse_args};

    // Pomocná funkce (není test): převede pole textových literálů &[&str] na Vec<String>
    // a zavolá parse_args. Hodí se, aby se v testech nemuselo pořád psát .to_owned().
    // values.iter()  – prochází prvky; každý prvek je tu typu &&str (odkaz na &str).
    // (*value)       – jedna dereference dá &str, .to_owned() z něj udělá String.
    // .collect()     – poskládá výsledky do Vec<String> (typ je uveden u proměnné args).
    fn parse(values: &[&str]) -> Result<Arguments, String> {
        let args: Vec<String> = values.iter().map(|value| (*value).to_owned()).collect();
        parse_args(&args)
    }

    // #[test] označí funkci jako test. Test projde, pokud funkce doběhne bez paniky (pádu).
    // assert_eq!(a, b) zpanikaří, když a != b, a vypíše obě hodnoty – proto Arguments
    // potřebuje Debug (výpis) a PartialEq (porovnání).
    // &["0", "XE15"] je odkaz na pole dvou textů, Rust ho automaticky převede na slice &[&str].
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

    // .unwrap() vybalí hodnotu z Ok (nebo zpanikaří u Err) – v testech je to běžné.
    // `.lot` pak čte pole struktury.
    #[test]
    fn accepts_four_character_lot_code() {
        assert_eq!(parse(&["628", "XE15"]).unwrap().lot, "XE15");
    }

    // Some(6) = šířka je nastavena na 6. None = šířka chybí.
    // assert!(podmínka) projde, když je podmínka pravdivá; .is_err() je true u Err.
    // Ověřuje se: doplnění nulami, přesná šířka, nula jako "nic", číslo delší než šířka
    // (chyba) a nenulové číslo bez nastavené šířky (chyba).
    #[test]
    fn pads_serial_to_configured_digit_count() {
        assert_eq!(format_serial_number(8960, Some(6)).unwrap(), "008960");
        assert_eq!(format_serial_number(123456, Some(6)).unwrap(), "123456");
        assert_eq!(format_serial_number(0, None).unwrap(), "0");
        assert!(format_serial_number(1234567, Some(6)).is_err());
        assert!(format_serial_number(12, None).is_err());
    }

    // Špatné vstupy: záporné ID ("-1" a navíc chybí lot), ID nad 5000, chybějící lot,
    // prázdný lot, nadbytečný argument a žádné argumenty. Všechno musí vrátit Err.
    // parse(&[]) = prázdné pole.
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
