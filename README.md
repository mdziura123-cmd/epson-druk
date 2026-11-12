# epson-druk

Prosty program w Ruście do drukowania bezprzewodowego na **Epson L3150** (i innych drukarkach sieciowych obsługujących IPP) z Linuksa – testowany z myślą o **Bazzite**.

Bez sterowników i bez zewnętrznych bibliotek – tylko biblioteka standardowa Rusta.

## Instalacja (gotowy pakiet)

1. Pobierz `epson-druk-linux-x86_64.tar.gz` z zakładki **Releases**.
2. Rozpakuj i zainstaluj w katalogu domowym:

```bash
tar -xzf epson-druk-linux-x86_64.tar.gz
mkdir -p ~/.local/bin && mv epson-druk ~/.local/bin/
```

Binarka jest statyczna (musl) – działa na każdej dystrybucji x86_64, także na niemodyfikowalnym Bazzite.

## Użycie

```bash
epson-druk szukaj                  # znajdź drukarki w sieci
epson-druk status  192.168.1.50    # stan, tusz, obsługiwane formaty
epson-druk drukuj  192.168.1.50 zdjecie.jpg
epson-druk zadania 192.168.1.50    # kolejka zadań
```

Uwaga: L3150 może nie przyjmować PDF bezpośrednio – sprawdź listę „Formaty” w `status`.

## Budowanie ze źródeł

```bash
curl --proto '=https' -sSf https://sh.rustup.rs | sh
cargo build --release
```

## Nowe wydanie

```bash
git tag v0.1.0 && git push origin v0.1.0
```

GitHub Actions zbuduje i opublikuje pakiet automatycznie.
