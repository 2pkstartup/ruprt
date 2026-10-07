// =============================================================================
// Komentovaná verze – kód je původní, přibyly jen české komentáře (// ...).
// Původní anglické komentáře (///) jsou ponechány beze změny.
//
// Program `rusend` je CLI nástroj: vezme zprávu (jeden argument NEBO data ze stdin),
// načte konfiguraci a pošle zprávu bajt po bajtu na tiskárnu přes TCP.
//
// Příklady použití:
//   rusend "^Q1"                  zpráva z argumentu
//   cat zprava.ezpl | rusend      zpráva z roury (stdin)
//   rusend < zprava.ezpl          zpráva z přesměrovaného souboru
//
// Návratové kódy: 0 = odesláno, 1 = chyba konfigurace nebo odeslání, 2 = chyba vstupu.
// =============================================================================

// `use` zpřístupní jména, aby šla psát krátce. Složené závorky { } importují víc věcí najednou.
// `ruprt` je knihovní část tohoto balíčku.
use ruprt::{config::AppConfig, print_minimal_help, printer::send_to_printer};
use std::{
    // Trait `Error` = společné rozhraní všech chybových typů (používá se v Box<dyn Error>).
    error::Error,
    // `self` importuje samotný modul std::io (takže lze psát io::stdin(), io::Error).
    // IsTerminal je trait, který přidává metodu .is_terminal().
    // Read je trait, který přidává čtecí metody (např. .read_to_end()).
    // Traity musí být importované, jinak jejich metody na hodnotách nejdou volat.
    io::{self, IsTerminal, Read},
    process,
};

/// Reads an exact payload from one argument or stdin; stdin is consumed to EOF.
// `mut stdin: impl Read` = parametr libovolného typu, který umí číst (implementuje trait Read).
// Díky tomu funkce funguje s opravdovým stdin i s náhražkou v testech (Cursor).
// `mut` je potřeba, protože čtení mění vnitřní stav zdroje (posouvá pozici).
// Návratový typ: Result<Vec<u8>, Box<dyn Error>>
//   Vec<u8>        = vektor bajtů (zpráva jako surová data, ne nutně platný text),
//   Box<dyn Error> = "krabice" s libovolnou chybou (dyn = typ se určí až za běhu).
//                    Díky tomu může funkce vracet různé druhy chyb pod jedním typem.
fn read_message(args: &[String], mut stdin: impl Read) -> Result<Vec<u8>, Box<dyn Error>> {
    // `match` na slice (pohled na pole) umí rozlišit podle počtu prvků:
    //   [message] = přesně jeden prvek (pojmenuje se `message`),
    //   []        = prázdné pole (žádný argument),
    //   _         = cokoliv jiného (dva a více prvků).
    let message = match args {
        // Jeden argument: text -> bajty (.as_bytes()) -> vlastněná kopie jako Vec<u8> (.to_vec()).
        [message] => message.as_bytes().to_vec(),
        // Žádný argument: čteme ze stdin.
        [] => {
            // Nový prázdný vektor; `mut`, protože do něj budeme zapisovat.
            // (Jméno `message` se tu znovu použije pro novou proměnnou jen uvnitř této větve.)
            let mut message = Vec::new();
            // Přečte všechna data až do konce vstupu (EOF) a přidá je do vektoru.
            // `&mut message` = půjčíme vektor s právem ho měnit.
            // `?` při chybě funkci hned ukončí a vrátí chybu; io::Error se automaticky
            // převede na Box<dyn Error>.
            stdin.read_to_end(&mut message)?;
            // Poslední výraz bloku bez středníku = hodnota celé větve `match`.
            message
        }
        // Dva a více argumentů: chyba. Větev nic nevrací, protože končí `return`.
        _ => {
            // io::Error::new(druh_chyby, text) vytvoří chybu; `.into()` ji převede do
            // Box<dyn Error>. Zápis `.into()` je na novém řádku jen kvůli formátování.
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "pass the message as one argument or through stdin",
            )
            .into());
        }
    };

    // Prázdná zpráva (např. prázdný vstup) nemá smysl posílat -> chyba.
    if message.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "message is empty").into());
    }

    // Řádek bez středníku = návratová hodnota funkce: úspěch se zprávou uvnitř.
    Ok(message)
}

/// Sends the supplied raw bytes to the configured printer without altering them.
fn main() {
    // Argumenty programu bez názvu programu (.skip(1)). `collect::<Vec<_>>()` je poskládá
    // do vektoru; `_` znamená "typ prvků si Rust odvodí sám" (tady String).
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    // Nápověda se ukáže jen když nejsou argumenty A ZÁROVEŇ stdin je interaktivní terminál
    // (tedy uživatel nic neposílá rourou ani přesměrováním). Jinak by program čekal na vstup.
    if args.is_empty() && io::stdin().is_terminal() {
        print_minimal_help(
            "rusend",
            // env!(...) se vyhodnotí při překladu: verze z Cargo.toml.
            env!("CARGO_PKG_VERSION"),
            "Sends an EZPL message to the configured printer over TCP.",
            "rusend <message> | command | rusend < message.ezpl",
            // \" je uvozovka uvnitř textu. Port 9100 je obvyklý port pro "raw" tisk po síti.
            "config.toml: printer_ip = \"192.0.2.10\", printer_port = 9100",
        );
        // `return;` ve funkci main ukončí program (s kódem 0).
        return;
    }

    // `&args` půjčí vektor jako slice. `io::stdin().lock()` zamkne stdin pro výhradní
    // (bufferované) čtení a výsledek umí Read, takže pasuje do `impl Read`.
    // Při chybě se text chyby vypíše na stderr (eprintln!) a program skončí s kódem 2.
    // process::exit má typ `!` ("nikdy se nevrátí"), proto se hodí do větve `Err`
    // vedle větve, která vrací hodnotu.
    let message = match read_message(&args, io::stdin().lock()) {
        Ok(message) => message,
        Err(error) => {
            eprintln!("Unable to read message: {error}");
            process::exit(2);
        }
    };

    // Načtení konfigurace (IP adresa a port tiskárny z config.toml).
    let config = match AppConfig::load() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("Failed to load config.toml: {error}");
            process::exit(1);
        }
    };
    // Odeslání. `&config` a `&message` jsou půjčené odkazy (funkce si data nebere do vlastnictví,
    // takže `message` můžeme dál použít, třeba pro .len()).
    // Při úspěchu vrací funkce adresu, kam se poslalo. {} je pozice (doplní message.len()),
    // {address} se doplní přímo z proměnné. Hlášky jdou na stderr, aby stdout zůstal čistý.
    match send_to_printer(&config, &message) {
        Ok(address) => eprintln!("Sent {} bytes to {address}", message.len()),
        Err(error) => {
            eprintln!("Failed to send message to printer: {error}");
            process::exit(1);
        }
    }
}

// #[cfg(test)] = tento blok se překládá jen při `cargo test`, do běžné binárky se nedostane.
#[cfg(test)]
mod tests {
    // `super` = nadřazený modul (tento soubor nahoře). Podmodul vidí i soukromé položky rodiče.
    use super::read_message;
    // Cursor obalí data v paměti tak, aby se chovala jako čtecí zdroj (implementuje Read).
    // V testech tak nahrazuje opravdový stdin.
    use std::io::Cursor;

    // #[test] označí funkci jako test; projde, když doběhne bez paniky.
    // První část: zpráva z argumentu. Cursor s textem "unused" se vůbec nepoužije,
    // takže výsledek musí být jen "^Q1".
    //   &["^Q1".to_owned()]  = odkaz na pole s jedním Stringem (převede se na slice).
    //   b"unused".to_vec()   = bajtový literál (b"...") převedený na vektor bajtů.
    // Druhá část: žádný argument (&[]) a data ze stdin musí projít beze změny,
    // včetně konců řádků \n. Porovnává se vektor bajtů s bajtovým literálem b"...".
    // .unwrap() vybalí hodnotu z Ok (u Err test zpanikaří).
    #[test]
    fn reads_message_from_argument_or_stdin_without_changing_bytes() {
        assert_eq!(
            read_message(&["^Q1".to_owned()], Cursor::new(b"unused".to_vec())).unwrap(),
            b"^Q1"
        );
        assert_eq!(
            read_message(&[], Cursor::new(b"^Q1\n^E\n".to_vec())).unwrap(),
            b"^Q1\n^E\n"
        );
    }

    // Chybové případy: prázdný stdin (Cursor::new(Vec::new()) = žádná data)
    // a dva argumenty najednou. assert!(...) projde, když je podmínka pravdivá;
    // .is_err() je true u Err.
    #[test]
    fn rejects_empty_message_and_multiple_arguments() {
        assert!(read_message(&[], Cursor::new(Vec::new())).is_err());
        assert!(read_message(&["a".to_owned(), "b".to_owned()], Cursor::new(Vec::new())).is_err());
    }
}