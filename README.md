RFA backup service.

### Build

```bash
## Install rust with the official guide
https://www.rust-lang.org/tools/install

## Clone the repository
git clone https://github.com/cncases/cases.git

## The compiled executable will be located at target/release/
cargo build -r
```

### Crawling website

`./target/release/spider`

More options:

```bash
RFA website crawler, downloading lists, pages and imgs

Usage: spider [OPTIONS]

Options:
  -w, --sites <SITES>    radio-free-asia,rfa-mandarin,rfa-cantonese,rfa-burmese,rfa-korean,rfa-lao,rfa-khmer,rfa-tibetan,rfa-uyghur,rfa-vietnamese
      --proxy <PROXY>    proxy (e.g., http://127.0.0.1:8089)
  -o, --output <OUTPUT>  [default: rfa_data]
  -h, --help             Print help
```

### Online service

`./target/release/web`

More options:

```bash
RFA backup website

Usage: web [OPTIONS]

Options:
  -a, --addr <ADDR>  listening address [default: 127.0.0.1:3333]
  -d, --data <DATA>  data folder, containing imgs/ and rfa.db/ [default: rfa_data]
  -h, --help         Print help
```

### Screenshot
![Screenshot](Screenshot.png)
