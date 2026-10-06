# ruprt

Sada sdílených Rust CLI aplikací pro práci s výrobními daty. Aplikace v tomto balíčku sdílejí MySQL konfiguraci a pomocné moduly. Aktuálně obsahuje binární aplikaci `rusn`.

## Požadavky

- Rust toolchain podporující edition 2024
- Přístup k MySQL databázi `specs` a odpovídajícím databázím `logs_XX`
- Dostupné funkce `specs.LOT_TO_DATE` a procedura `specs.FIND_MAX_SERIAL_NUMB`

## Konfigurace

V kořeni projektu vytvoř lokální `config.toml` podle šablony:

```bash
cp config.example.toml config.toml
```

Uprav `mysql_url`:

```toml
mysql_url = "mysql://USER:PASSWORD@HOST:3306/"
```

Do URL doplň přihlašovací údaje a adresu MySQL serveru. Soubor `config.toml` je ignorovaný Gitem; necommituj ho, protože může obsahovat heslo. Aplikace hledá konfiguraci v adresářích nad spustitelným souborem nebo aktuálním pracovním adresářem.

Použitý MySQL účet potřebuje příslušná oprávnění pro čtení serializačních a logovacích tabulek a spouštění databázových funkcí/procedur.

## `rusn`

`rusn` převede projekt a lot na datum pomocí `specs.LOT_TO_DATE`, zjistí linky s odpovídajícími záznamy a zavolá `specs.FIND_MAX_SERIAL_NUMB` pro každou z nich. Vrátí nejvyšší nalezené sériové číslo.

Sestavení a spuštění z kořene projektu:

```bash
cargo run --release --bin rusn -- 628 XE15
```

Samostatná release binárka:

```bash
cargo build --release --bin rusn
./target/release/rusn 628 XE15
```

Syntaxe:

```text
rusn <project_ID> <lot>
```

Například `rusn 628 XE15`. `project_ID` musí být celé číslo od `0` do `5000`; lot musí být neprázdný řetězec. Při spuštění bez argumentů aplikace vypíše stručnou nápovědu.

### Výstup

Úspěšné spuštění vypíše na standardní výstup pouze číslo:

- `-1` – projekt není serializovaný
- `0` – lot nemá přiřazené datum nebo nebylo nalezeno sériové číslo
- kladné číslo – nejvyšší nalezené sériové číslo

Chyby konfigurace nebo databázového připojení se vypisují na standardní chybový výstup a aplikace skončí s nenulovým návratovým kódem.

## `ruprt`

Načte poslední EZPL zprávu pro projekt a linku z `mess.tbl_mess`, dekóduje `mess_64` z Base64 a vypíše příkazy `2XX||` bez jejich databázových prefixů.

```bash
cargo run --bin ruprt -- 628 -l 4 -d 261001
```

Syntaxe je `ruprt <project_ID> [-l line] [-d YYMMDD]`. Linka může být zadána přes `-l`; jinak se použije `default_line` z `config.toml`. Datum `-d` je nepovinné a bez něj se použije dnešní datum. Z `specs.tbl_valves` se načte projektový `DateCode` a procedura `specs.LOT` vypočítá odpovídající lot.

Aktuálně se vypočtený lot připraví, ale ještě se nedosazuje do EZPL. Plánované tiskové přepínače `-p`, `-h`, `-s`, `-c`, `-x`, `-y`, `-r` a `-q` zatím nejsou implementované.

## Vývoj

Spuštění testů a kontrola formátování:

```bash
cargo test
cargo fmt -- --check
```
