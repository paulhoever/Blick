# Blick

Blick ist ein Bildbetrachter für GNOME, geschrieben in Rust mit GTK 4 und libadwaita. Ein Ordner erscheint als Kontaktbogen: alle Bilder dicht nebeneinander, in der Reihenfolge des Ordners durchnummeriert. Ausgewählte Bilder lassen sich gemeinsam drehen oder zu Immich hochladen, ein einzelnes in Helligkeit, Kontrast und Sättigung korrigieren.

![Kontaktbogen mit Bibliothek](docs/screenshots/kontaktbogen.png)

## Was Blick kann

Der Kontaktbogen zeigt standardmäßig den ganzen Ordnerbaum, der Ordner-Knopf in der Kopfleiste schaltet auf die oberste Ebene um. Thumbnails liegen in `~/.cache/blick/thumbs` und entstehen in eigenen Threads, die zuletzt angefragte Kachel zuerst. Neue und gelöschte Dateien erscheinen ohne Neuladen, bis 1000 Ordner tief.

Die Bibliothek besteht aus dem Bilder-Ordner (beim ersten Start schon drin) und allem, was du über „Ordner öffnen“ dazuholst, auch per `blick -r ~/Fotos`. Jeder Unterordner mit Bildern wird ein Album. Versteckte Ordner, Synologys `@eaDir` und Papierkörbe lässt Blick aus.

![Einzelansicht mit Stil und Korrekturen](docs/screenshots/bearbeiten.png)

### Formate

JPEG, PNG, WebP, GIF, BMP und TIFF lädt Blick selbst. Kamera-RAW (CR2, CR3, NEF, ARW, RAF, RW2, ORF, DNG und rund 20 weitere) läuft über rawler: zum Ansehen die eingebettete Vorschau, für Export und Zoom die volle Entwicklung. Deren Tonwerte gleicht Blick pro Kanal an die Kamera-Vorschau an, sonst wirkt das Bild flacher als auf dem Kameradisplay. HEIC, AVIF, JPEG XL, SVG, EXR und mehr kommen über glycin aus der GNOME-Laufzeit, das jede Datei in einem abgeschotteten Prozess dekodiert.

Videos (MP4, MOV, MKV, WebM, AVI, 3GP, MTS und weitere) stehen mit Standbild und Abspiel-Plakette im Bogen und laufen in der Einzelansicht. Jedes startet stumm und in Schleife. Untertitel und Spulen gibt es bewusst nicht. Abgespielt wird über GStreamer aus der Laufzeit (H.264, HEVC, AV1, VP9, mit VA-API, wo vorhanden), Drehungs-Tags von Handyvideos beachtet Blick. Aus Immich kommt dessen Fassung für den Browser.

### Einzelansicht

Blätter- und Zoomknöpfe liegen über dem Bild. Gezoomt wird mit Strg+Mausrad, Pinch, Doppelklick oder `+` `-` `0` `1`, Vollbild ist F11, Bildinfos aus EXIF öffnet Alt+Enter. Die Nachbarbilder lädt Blick im Hintergrund vor.

Drehen geht einzeln oder für die Auswahl, mit Rückgängig. JPEG dreht verlustfrei über das EXIF-Tag. RAW, HEIC, AVIF und JPEG XL bekommen eine XMP-Datei daneben (`<datei>.xmp`), das Original bleibt bitgleich. Andere Formate kodiert Blick neu, mit EXIF und ICC. Animierte GIFs lässt es, wie sie sind.

### Zuschneiden

Frei oder mit Vorlage: Original, 1:1, 4:3, 3:2, 16:9, 5:4, 7:5 oder 2:1, jeweils quer oder hoch. Den Rahmen ziehst du an Ecken und Kanten oder verschiebst ihn von innen, die Größe in Pixeln steht in der Seitenleiste. Wie die Korrekturen landet der Zuschnitt in einer Kopie, im Original oder gemerkt in `<bild>.xmp`. Dreht man das Bild danach, dreht der gemerkte Zuschnitt mit.

![Zuschneiden mit Vorlage 3:2 im Hochformat](docs/screenshots/zuschneiden.png)

### Korrekturen und Stile

Helligkeit, Kontrast und Sättigung lassen sich mit Live-Vorschau einstellen. Dazu kommen Bildstile aus `.cube`-LUTs, HaldCLUT-PNGs und Lightroom-Presets (`.xmp`) mit Stärkeregler. Enthält ein Preset etwas, das Blick nicht abbilden kann, sagt der Import es dir. Die Filmsimulationen von RawTherapee (Kodak, Fuji, Ilford und andere, CC BY-SA 4.0) lädt Blick auf Wunsch selbst herunter.

Das Ergebnis speicherst du als Kopie, merkst es als `<bild>.xmp` daneben oder exportierst den Stil als `.cube`. Ein Stil lässt sich auch auf viele Bilder auf einmal anwenden. Gemerkte Korrekturen und Stile zeigt der Kontaktbogen ebenfalls.

### Aufräumen und Dubletten

Entf verschiebt ins Papierkorb, mit Rückgängig. Endgültig löscht Blick nur in der Dublettensuche und nur, wenn du es ausdrücklich wählst (für Freigaben ohne Papierkorb).

Die Dublettensuche (Hauptmenü, Strg+Umschalt+D) ist an dupeGuru angelehnt und steckt in der Bibliothek `blick-dupes`. Sie kennt vier Modi: ähnliche Bilder, identischer Inhalt, Dateinamen und Ordner. Suchen kann sie in lokalen Ordnern, auf Netzwerkfreigaben über GIO/gvfs (`smb://`, `sftp://`, `davs://`, `nfs://`) und in der Immich-Bibliothek. Orte lassen sich als Referenz markieren, die Blick nie löscht.

Beim Inhaltsvergleich zählt erst die Größe, dann ein blake3-Hash über Anfang, Mitte und Ende (auf Servern liest Blick nur diese Bereiche), dann der volle Hash. Bei Bildern vergleicht es 15×15-Farbblöcke wie dupeGuru und sucht über einen Gradientenhash in vier 16-Bit-Streifen, statt jedes Bild mit jedem zu vergleichen. Gedrehte Treffer und andere Seitenverhältnisse sind optional. Immich liefert SHA-1 gleich mit, lokale Dateien gleicher Größe prüft Blick dagegen ohne Download. Hashes und Signaturen liegen in `~/.cache/blick/dupes.db`. Zum Aufräumen markierst du Treffer, vergleichst sie nebeneinander und schickst sie in den Papierkorb (bei Immich in dessen Papierkorb), verschiebst sie, ersetzt sie durch Hardlinks oder exportierst eine CSV.

Hell oder Dunkel schaltest du im Hauptmenü um, unabhängig vom System.

### Immich

Blick spricht mit Immich ab 2.x, geprüft gegen die API von 3.3.1. Beim Hochladen schickt es eine Prüfsumme mit, damit der Server Dubletten erkennt, auf Wunsch direkt in ein Album. Alben und Zeitleiste blätterst du monatsweise durch, Originale lädst du herunter. In der Einzelansicht kommt nach der Vorschau die volle Auflösung: bei JPEG und Ähnlichem über `size=fullsize`, bei HEIC und RAW lädt Blick das Original und dekodiert es selbst, solange der Server keine Fullsize-Bilder erzeugt.

## Tastenkürzel

| Kürzel | Wirkung |
|---|---|
| Strg+O | Ordner öffnen |
| Strg+R / Strg+Umschalt+R | Auswahl oder aktuelles Bild nach rechts / links drehen |
| Strg+U | Auswahl zu Immich hochladen |
| Enter, Doppelklick, Leertaste | Bild groß öffnen (Leertaste in der Einzelansicht: zurück zum Bogen) |
| Pfeil links / rechts | Blättern in der Einzelansicht |
| E | Bearbeiten ein/aus |
| Linke Maustaste aufs Bild halten (beim Bearbeiten) | Original zeigen, solange gedrückt |
| S | Stil wählen |
| C | Zuschneiden: Pfeile verschieben den Rahmen, mit Umschalt ändern sie die Größe, Enter übernimmt, Esc bricht ab |
| `+` `-` `0` `1` | Zoom: größer, kleiner, einpassen, 100 % |
| Leertaste, K (Video) | Abspielen/Anhalten |
| M (Video) | Ton an/aus |
| Pfeil hoch / runter (Video) | Lauter / leiser |
| F11 | Vollbild |
| Alt+Enter | Bildinfos |
| Entf | In den Papierkorb |
| Strg+, | Einstellungen |
| Strg+? | Alle Tastenkürzel |

## Installation

Blick läuft als Flatpak (GNOME 51). Das Paket `blick.flatpak` hängt an jedem Release auf der [Releases-Seite](https://github.com/paulhoever/Blick/releases), die Prüfsumme liegt daneben.

```sh
flatpak install --user blick.flatpak
flatpak run io.github.paulhoever.Blick
```

## Immich einrichten

In Immich legst du unter Kontoeinstellungen einen API-Schlüssel an. Er braucht `user.read`, `album.read`, `asset.read`, `asset.view`, `asset.download`, `asset.upload` und `albumAsset.create`. In Blick trägst du unter Einstellungen Adresse und Schlüssel ein und drückst „Prüfen und speichern“. Der Schlüssel liegt danach im Schlüsselbund (oo7, im Flatpak über das Secret-Portal), die Adresse in GSettings.

Zum Testen nimmst du besser kein echtes Konto. `tools/immich_mock.py` spielt einen Immich-Server mit den Bildern eines beliebigen Ordners:

```sh
python3 tools/immich_mock.py ~/Testbilder 2291
BLICK_IMMICH_URL=http://127.0.0.1:2291 BLICK_IMMICH_KEY=demo-key cargo run
```

Die beiden Umgebungsvariablen haben Vorrang vor den gespeicherten Einstellungen.

## Aufbau

```
crates/
  blick-core/    Bildverarbeitung ohne GTK: Dekodieren, Korrekturen, Drehen, Caches, Batch
  blick-immich/  Immich-Client (reqwest, rustls mit ring), ohne GTK
  blick-dupes/   Dublettensuche: Quellen (lokal, GIO, Immich), Pipeline, Cache, Ergebnisse
  blick/         die App: Fenster, Kontaktbogen, Einzelansicht, Dialoge
data/            Desktop-Datei, Metainfo, GSettings-Schema, CSS, Icons
build-aux/       Flatpak-Manifest und die Liste der Crates für den Offline-Build
tools/           Immich-Attrappe für Tests
```

Im UI-Thread passiert keine Datei- oder Netzwerkarbeit. Dekodieren, Drehen und Speichern laufen über `gio::spawn_blocking` oder die Thumbnail-Threads, das Netzwerk auf einer eigenen tokio-Runtime. Die Ergebnisse kommen per Future zurück in den GTK-Hauptloop. Jede Ansicht zählt eine Generation mit: Ein spätes Ergebnis für ein Bild, von dem du schon weitergeblättert hast, wird verworfen.

## Bauen

Für die Entwicklung reichen Rust ab 1.85 und die Entwicklerpakete von GTK ab 4.14 und libadwaita ab 1.5:

```sh
sudo apt install libgtk-4-dev libadwaita-1-dev
cargo run -- ~/Bilder
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Das Flatpak baut ohne Netz, mit Rust aus der SDK-Extension:

```sh
flatpak install --user flathub org.gnome.Sdk//51 org.freedesktop.Sdk.Extension.rust-stable//26.08
flatpak-builder --user --force-clean --install build-dir build-aux/io.github.paulhoever.Blick.yml
flatpak run io.github.paulhoever.Blick
```

Nach jeder Änderung an `Cargo.lock` muss `build-aux/cargo-sources.json` neu entstehen:

```sh
python3 -m venv .venv-tools && .venv-tools/bin/pip install aiohttp tomlkit
curl -LO --output-dir .venv-tools https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/master/cargo/flatpak-cargo-generator.py
.venv-tools/bin/python .venv-tools/flatpak-cargo-generator.py Cargo.lock -o build-aux/cargo-sources.json
```

Ein Installationspaket für andere Rechner:

```sh
flatpak-builder --user --force-clean --repo=repo build-dir build-aux/io.github.paulhoever.Blick.yml
flatpak build-bundle repo blick.flatpak io.github.paulhoever.Blick --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo
```

## Grenzen

- HEIC, AVIF und JXL dreht Blick nur im Sidecar. Korrekturen lassen sich dort nicht überschreiben, nur als Kopie speichern: Der HEIF-Encoder der GNOME-Laufzeit schreibt kein EXIF, das Aufnahmedatum ginge verloren.
- Der RAW-Tonabgleich nähert die Kamera-Vorschau an, eine Kamera-Tonkurve ist es nicht. Ohne eingebettete Vorschau bleibt die Entwicklung flach.
- Lightroom rechnet Lichter, Tiefen, Weiß und Schwarz lokal, Blick global. Presets sehen ähnlich aus, aber nicht gleich.
- Thumbnails nach der Freedesktop-Spezifikation (geteilt mit Nautilus) fehlen. Im Flatpak bräuchte das Zugriff auf `~/.cache/thumbnails`.
- Es gibt noch keine Übersetzungen (gettext), die Oberfläche ist deutsch.

## Lizenz

GPL-3.0-or-later
