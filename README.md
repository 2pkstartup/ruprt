# ruprt

Sada sdílených Rust CLI aplikací pro práci s výrobními daty. Aplikace v tomto balíčku sdílejí konfiguraci a pomocné moduly. Obsahuje aplikace `rusn`, `ruprt` a `rusend`.

Každá aplikace vypisuje podrobnou nápovědu s popisem argumentů a příklady přes `--help`; `rusn` a `rusend` podporují také `-h`. U `ruprt` je `-h` vyhrazeno pro teplotu tisku.

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

`rusend` navíc potřebuje printer destination v `config.toml`:

```toml
printer_ip = "192.0.2.10"
printer_port = 9100
printer_id = 2
```

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

Dlouhá nápověda:

```bash
rusn --help
```

Lot se přes `specs.LOT_TO_DATE` převede na datum. Šestimístný číselný lot nelze předat jako lot, protože `-d`/lotové rozhraní rozpoznává šest číslic jako `YYMMDD`.

### Výstup

Úspěšné spuštění vypíše na standardní výstup pouze číslo:

- `-1` – projekt není serializovaný
- `0` – lot nemá přiřazené datum nebo nebylo nalezeno sériové číslo
- kladné SN – nejvyšší nalezené číslo, doplněné nulami zleva na délku `specs.tbl_coding.digit_count`

Chyby konfigurace nebo databázového připojení se vypisují na standardní chybový výstup a aplikace skončí s nenulovým návratovým kódem.

## `rulot`

Převede lot na datum pomocí `specs.LOT_TO_DATE` a vypíše pouze datum ve formátu `YYMMDD`:

```bash
cargo run --release --bin rulot -- 945 XK03
cargo run --release --bin rulot -- 945 -d XK03
```

Project ID musí být celé číslo `0–5000`. Při spuštění bez argumentů se zobrazí nápověda; nenalezený lot nebo databázová chyba se vypíše na `stderr` a aplikace skončí s nenulovým kódem.

## `ruprt`

Načte poslední EZPL zprávu pro projekt a linku z `mess.tbl_mess`, dekóduje `mess_64` z Base64, vypočítá projektový lot a vypíše příkazy `2XX||` bez jejich databázových prefixů.

```bash
cargo run --bin ruprt -- 628 -l 4 -d 261001
cargo run --bin ruprt -- 945 -l 1 -d XK03 -p 3 -t
cargo run --bin ruprt -- --help
```

Syntaxe je `ruprt <project_ID|message.clf> [message.clf] [--raw] [-t [test]] [-l line] [-d YYMMDD|lot] [-p count] [-h temp] [-s speed] [-e value] [-r value] [-q value] [-x offset] [-y offset]`. Linka může být zadána přes `-l`; jinak se použije `default_line` z `config.toml`. Bez `-d` se použije dnešní datum. Z `specs.tbl_valves` se načte projektový `DateCode` a procedura `specs.LOT` vypočítá odpovídající lot. Argument `-d` přijímá také lot, například `-d XK03`; ten se přes `specs.LOT_TO_DATE` převede na datum. Šestimístná číselná hodnota se vždy chápe jako datum `YYMMDD`; ostatní neprázdné hodnoty jako lot.

Místo DB zprávy lze předat exportovaný `.clf` soubor. Project ID se vezme z argumentu, nebo z řádku `008||`; jsou-li uvedeny obě hodnoty, musí být shodné. Po úspěšném fyzickém tisku se upravená zpráva uloží jako Base64 do aktuální měsíční `mess.tbl_auto_YYYY` (`valid=0`, `desc=autosave`). Jméno se převezme z `009||`; pokud chybí, uloží se `N/A`. Chybějící `008||` se při autosave doplní jako `0000`. `printer` se vezme z `printer_id` v configu. Testní režim `-t` do autosave nic neukládá.

Soubor lze vytisknout i samotným `ruprt label.clf`, bez project ID a linky. Tento režim ponechá lot, QR/DataMatrix a SN ze souboru; nevolá jejich databázové procedury. Odstraní prefixy a komentáře `200||`, případné zadané tiskové přepínače použije. Chybějící projekt a linka mají pro autosave hodnotu `0` (linka jinak použije `default_line`).

```bash
cargo run --bin ruprt -- label.clf
cargo run --bin ruprt -- label.clf -v
```

`-v` vypne autosave; nevyžaduje pak ani `printer_id`. Bez `-v` zůstává zápis do měsíční autosave tabulky po úspěšném TCP odeslání. TCP odeslání samo nepotvrzuje fyzické vytištění štítku.

Přepínač `--raw` uloží poslední zprávu pro projekt/linku do aktuálního adresáře jako `<projectID>_<YY-MM-DD HH:MM:SS>.clf`. Provede pouze Base64 dekódování `mess_64`; nepočítá lot ani SN a nemění EZPL.

Vypočtený lot se dosadí do řádku `210`. Obsahuje-li zpráva řádek `225`, aplikace zavolá `specs.SERNUM` s project ID a původní linkou a celý řádek nahradí vráceným QR/DataMatrix payloadem. Volitelné tiskové parametry mění hodnoty v EZPL; neuvedené parametry ponechají původní hodnotu:

- `-p count`: počet výtisků `1–20000`, nahrazuje hodnotu za `^P` v příkazu `204`
- `-h temp`: teplota `0–20`, nahrazuje hodnotu za `^H` v příkazu `201`
- `-s speed`: rychlost `2–6`, nahrazuje hodnotu za `^S` v příkazu `201`
- `-e value`: hodnota `-40–40`, nahrazuje hodnotu za `^E` v příkazu `201`
- `-r value`: horizontální posun layoutu `0–100`, nahrazuje hodnotu za `^R` v příkazu `201`
- `-q value`: vertikální posun layoutu `-100–100`, nahrazuje hodnotu za `~Q` v příkazu `201`
- `-x offset`, `-y offset`: podepsaný posun souřadnic v příkazech `231` a `232`; výsledná souřadnice neklesne pod nulu

Po úspěšném zpracování se zpráva standardně odešle na tiskárnu nastavenou v configu. Přepínač `-t` (také `-t test`) zapne testní režim: zprávu pouze vypíše na konzoli a na tiskárnu ji nepošle.

## `rusend`

Odešle EZPL zprávu raw TCP spojením na tiskárnu. Zprávu lze zadat jako jediný argument, nebo ji předat přes stdin:

```bash
cargo run --bin rusend -- "^Q1\n^W40\n..."
cat label.ezpl | cargo run --bin rusend
```

Konfigurace cíle je ve společném `config.toml` pod `printer_ip` a `printer_port`. Bez argumentu při interaktivním spuštění vypíše aplikace nápovědu; z prázdného stdin zprávu neodešle. Úspěšné odeslání nevypisuje do stdout žádný text, aby se tisková zpráva nemíchala s diagnostikou.

```bash
rusend --help
```

## Vývoj

Spuštění testů a kontrola formátování:

```bash
cargo test
cargo fmt -- --check
```
