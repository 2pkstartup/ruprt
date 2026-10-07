// =============================================================================
// Komentovaná verze – kód je původní, přibyly jen české komentáře (// ...).
// Původní anglické komentáře (/// a //) jsou ponechány beze změny.
//
// Tento soubor je KNIHOVNÍ MODUL (ne program): nemá main(). Jeho struktura AppConfig
// používají programy rusn a rusend (viz `use ruprt::config::AppConfig`).
// Načítá nastavení ze souboru config.toml, který hledá nad umístěním programu
// a nad aktuálním adresářem.
//
// Příklad config.toml (jen mysql_url je povinný, ostatní klíče jsou volitelné):
//   mysql_url    = "mysql://USER:PASSWORD@HOST:3306/"
//   default_line = 1
//   printer_ip   = "192.0.2.10"
//   printer_port = 9100
//   printer_id   = 3
//
// Pozor: mysql_url obsahuje heslo v čitelné podobě, takže config.toml
// nepatří do gitu (přidej ho do .gitignore).
// =============================================================================

// Makro/trait `Deserialize` z knihovny serde umí převést data z nějakého formátu (tady TOML)
// na Rust strukturu.
use serde::Deserialize;
// Import několika věcí ze standardní knihovny najednou:
//   env          = práce s prostředím procesu (cesta ke spustitelnému souboru, aktuální adresář),
//   error::Error = společný trait chyb (pro Box<dyn Error>),
//   fs           = souborový systém (čtení souborů),
//   io           = vstup/výstup, mj. typ io::Error,
//   Path         = "půjčená" cesta (jako &str pro text), PathBuf = vlastněná cesta (jako String).
use std::{
    env,
    error::Error,
    fs, io,
    path::{Path, PathBuf},
};

// #[derive(Debug, Deserialize)] vygeneruje kód pro:
//   Debug       – výpis přes {:?},
//   Deserialize – vytvoření struktury z TOML: serde přiřadí klíče z TOML k polím podle jmen.
// Klíče v TOML se tedy musí jmenovat stejně jako pole (mysql_url, printer_ip, ...).
#[derive(Debug, Deserialize)]
/// Shared connection settings loaded by every binary in this Cargo package.
// `pub` = struktura je veřejná, viditelná i z jiných souborů a programů. Každé pole má
// vlastní `pub`, jinak by bylo soukromé (u struktur se `pub` u polí píše zvlášť).
pub struct AppConfig {
    /// MySQL URL, including credentials, host, and optional default database.
    // String bez Option = pole je POVINNÉ. Když v config.toml chybí, načtení selže chybou.
    pub mysql_url: String,
    /// Optional production line used when the CLI does not specify `-l`.
    // Option<u32> = hodnota nemusí existovat (None), jinak Some(číslo).
    // #[serde(default)] říká: když klíč v TOML chybí, použij výchozí hodnotu typu
    // (u Option je to None). U polí typu Option by serde chybějící klíč vzal jako None
    // i bez tohoto atributu; atribut to zapisuje výslovně.
    #[serde(default)]
    pub default_line: Option<u32>,
    /// IP address of the raw TCP printer used by `rusend`.
    #[serde(default)]
    pub printer_ip: Option<String>,
    /// TCP port of the raw printer service.
    // u16 = celé číslo bez znaménka, 16 bitů (0 až 65535), což přesně odpovídá rozsahu TCP portů.
    #[serde(default)]
    pub printer_port: Option<u16>,
    /// Numeric printer identifier stored in autosave records.
    #[serde(default)]
    pub printer_id: Option<u32>,
}

// `impl AppConfig { ... }` = blok s metodami a funkcemi patřícími k typu AppConfig.
// Uvnitř znamená `Self` totéž co `AppConfig`.
impl AppConfig {
    /// Finds `config.toml` above the executable or working directory and loads it.
    /// This also lets binaries launched from `target/release` find the project config.
    // Funkce bez parametru `self` se volá přes typ: AppConfig::load() (jako "statická" metoda).
    // Result<Self, Box<dyn Error>> = Ok(AppConfig) nebo Err(libovolná chyba v "krabici").
    pub fn load() -> Result<Self, Box<dyn Error>> {
        // load_with_path() vrací dvojici (nastavení, cesta). Result::map změní jen hodnotu uvnitř Ok;
        // chyba (Err) projde beze změny. Closure |(config, _)| rovnou rozebere dvojici:
        // `config` si ponechá, `_` znamená "tuhle část zahodit" (cestu nepotřebujeme).
        Self::load_with_path().map(|(config, _)| config)
    }

    /// Loads the config and returns the path used to find it.
    // Vrací dvojici (tuple) v Ok: nastavení a cestu k nalezenému souboru.
    pub fn load_with_path() -> Result<(Self, PathBuf), Box<dyn Error>> {
        // Prázdný rostoucí vektor adresářů. `mut` = půjde do něj přidávat.
        // Typ prvků (PathBuf) si Rust odvodí z toho, co do něj níže přidáme.
        let mut search_directories = Vec::new();

        // Prefer the executable's ancestors, then fall back to the launch directory.
        // `if let Ok(x) = ...` = proveď blok jen tehdy, když výraz vrátil Ok; hodnota se
        // pojmenuje `x`. Při Err se blok přeskočí (chyba se tiše ignoruje).
        // env::current_exe() = cesta k právě běžícímu programu.
        if let Ok(executable) = env::current_exe() {
            // .parent() = adresář, ve kterém soubor leží. Vrací Option (kořen nemá rodiče),
            // proto opět `if let Some(...)`.
            if let Some(directory) = executable.parent() {
                // .ancestors() = iterátor: samotný adresář, jeho rodič, rodič rodiče, ... až ke kořeni.
                // .map(Path::to_path_buf) = každý prvek (půjčená Path) převede na vlastněný PathBuf.
                // (Předává se přímo jméno funkce místo closure.)
                // .extend(...) připojí všechny prvky iterátoru na konec vektoru.
                search_directories.extend(directory.ancestors().map(Path::to_path_buf));
            }
        }

        // Totéž pro aktuální pracovní adresář (odkud byl program spuštěn). Přidá se za
        // adresáře od programu, takže ty mají přednost.
        if let Ok(directory) = env::current_dir() {
            search_directories.extend(directory.ancestors().map(Path::to_path_buf));
        }

        // Řetězec iterátorů, čte se po krocích:
        //  .iter()        – projde adresáře (půjčuje je, vektor zůstane),
        //  .map(...)      – ke každému adresáři přidá "config.toml" (.join složí cestu),
        //  .find(...)     – vrátí PRVNÍ cestu, která je opravdu soubor (is_file()). Vrací Option.
        //                 Iterátory jsou "líné": po nalezení prvního souboru se dál nehledá.
        //  .ok_or_else(..) – Some(cesta) -> Ok(cesta), None -> Err(chyba "nenalezeno").
        //                 Closure se zavolá jen v případě chyby.
        //  ?              – při Err funkce skončí; io::Error se automaticky převede na Box<dyn Error>.
        let config_path = search_directories
            .iter()
            .map(|directory| directory.join("config.toml"))
            .find(|path| path.is_file())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "config.toml not found above executable or working directory",
                )
            })?;

        // Načtení a zpracování souboru (viz from_path níže). &config_path jen půjčuje cestu.
        let config = Self::from_path(&config_path)?;
        // Výsledek: dvojice (nastavení, cesta). Tady se config_path PŘEDÁVÁ volajícímu
        // (přesun vlastnictví), proto se dřív používal jen půjčený odkaz.
        Ok((config, config_path))
    }

    /// Loads and deserializes a TOML config file at an explicit path.
    // `path: impl AsRef<Path>` = parametr libovolného typu, který jde přečíst jako cestu:
    // &str, String, &Path, PathBuf i &PathBuf. Volající tak nemusí nic převádět.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, Box<dyn Error>> {
        // Přečte celý soubor do Stringu (musí to být platný UTF-8). `?` ukončí funkci při chybě
        // (např. soubor neexistuje nebo nejsou práva).
        let contents = fs::read_to_string(path)?;
        // toml::from_str zpracuje text jako TOML a vyrobí z něj hodnotu požadovaného typu.
        // Typ (Self = AppConfig) Rust pozná z návratového typu funkce. Chybějící povinný klíč
        // nebo špatný typ hodnoty (např. text místo čísla) skončí chybou, kterou `?` vrátí.
        // &contents půjčuje text jen pro čtení.
        Ok(toml::from_str(&contents)?)
    }
}