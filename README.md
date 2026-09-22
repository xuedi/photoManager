# photoManager

A GNOME desktop application for managing the data of a personal photo library: see what is
missing (location, dates, tags, people), fix it in bulk with a preview before every change,
bring the folder structure and file names into shape, and import new photos.

The photos themselves are the only source of truth. Everything the application knows is read
from the files (EXIF, IPTC, XMP) and can be thrown away and rebuilt at any time. Nothing is
written without a preview that was confirmed.

## Build and run

Needs a Rust toolchain, GTK 4, libadwaita, `blueprint-compiler`, `just` and `exiftool`.

```bash
just build     # debug build
just run       # run it
just check     # format, lint, test
```

By default the library is expected at `~/Nextcloud/Photos`; `PHOTOMANAGER_LIBRARY` points it
somewhere else.

## Data from others

Place names and country outlines come from GeoNames, under CC BY 4.0. See [NOTICE](NOTICE).

## Documentation

Architecture notes per subsystem: [docs/README.md](docs/README.md).
