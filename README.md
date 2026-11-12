# epson-druk

Prosty program w Ruście do drukowania bezprzewodowego na **Epson L3150** (i innych drukarkach sieciowych obsługujących IPP) z Linuksa – testowany z myślą o **Bazzite**.

Bez sterowników i bez zewnętrznych bibliotek – tylko biblioteka standardowa Rusta.

## Aplikacja z okienkiem (AppImage)

Pobierz **`Epson-Druk-x86_64.AppImage`** z zakładki **Releases**, nadaj prawo uruchamiania i kliknij dwa razy:

```bash
chmod +x Epson-Druk-x86_64.AppImage
./Epson-Druk-x86_64.AppImage
```

Na Bazzite najwygodniej dodać ją do menu przez **Gear Lever** (Flathub).

Co potrafi:
- sama znajduje drukarkę w sieci i ją zapamiętuje,
- pokazuje stan drukarki, ostrzeżenia (np. brak papieru) i poziom tuszu, jeśli drukarka go podaje,
- drukowanie przez przeciągnięcie plików albo przycisk „Wybierz pliki…” (Ctrl+O, Ctrl+P),
- liczba kopii, kolor / czarno-biało, dopasowanie do strony,
- PDF-y zamienia na obrazy, gdy drukarka nie przyjmuje PDF (potrzebny `pdftoppm` lub `gs` w systemie),
- PNG, WEBP, BMP i GIF zamienia na JPEG,
- kolejka zadań z możliwością anulowania.

Jeśli okno jest puste (zdarza się na kartach NVIDIA), aplikacja już ustawia `WEBKIT_DISABLE_DMABUF_RENDERER=1`; zgłoś problem w Issues.

## Wersja terminalowa (gotowy pakiet)

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

Na GitHubie: **Releases → Create a new release**, wpisz nowy tag (np. `v0.2.0`) i kliknij **Publish release**.
GitHub Actions zbuduje AppImage oraz wersję terminalową i doda je do wydania.
