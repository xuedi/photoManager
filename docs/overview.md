# Overview

photoManager is two crates in one cargo workspace.

```mermaid
flowchart LR
    subgraph app [app]
        main[main] --> application
        application --> window[window<br/>view switcher]
        application -. devtools feature .-> devtools[dev actions]
        window --- blueprint[(Blueprint UI<br/>compiled into GResource)]
    end
    subgraph core [core]
        paths
        scan --> cache[(cache)]
        scan --> metadata
        scan --> identity
        scan --> layout
        fixtures[fixtures<br/>test data]
    end
    application --> paths
    window --> library[library<br/>scan off the main thread]
    library --> scan
    photos[(photo library)] -. read .-> scan
```

`core` holds everything that does not need a display: where things live on disk, reading a
photo's metadata, identifying it by its image data, reading its folder names, the scan and the
cache it fills. It has no GTK dependency, so it can be tested without a session. What the cache
holds and how a scan works: [cache.md](cache.md).

`app` holds the GTK application: the window, its views, and the actions they expose. The user
interface is written in Blueprint, compiled to GtkBuilder XML by the build script and bundled
as a GResource, so the binary carries its own interface.

## Where data lives

| What | Where | Lifetime |
|------|-------|----------|
| photo library | `~/Nextcloud/Photos`, or `PHOTOMANAGER_LIBRARY` | the truth, never rewritten without a confirmed preview |
| cache | `$XDG_CACHE_HOME/org.beijingcode.PhotoManager` | disposable, rebuilt from the photos |
| settings, presets, journal | `$XDG_DATA_HOME/org.beijingcode.PhotoManager` | kept |

Every location is resolved in one place, from the environment. Pointing `HOME`, `XDG_*` and
`PHOTOMANAGER_LIBRARY` somewhere else moves the whole application, which is how tests keep away
from real photos. In a debug build the application refuses to start when the library is not
there instead of creating one.

## Actions

The window and the application expose what they can do as actions, which is what menus,
shortcuts and tests all use:

| Action | Does |
|--------|------|
| `win.show-view` | show one of `dashboard`, `gallery`, `tools`, `suggestions` |
| `win.scan` | read what changed in the library into the cache |
| `win.cancel-scan` | stop a running scan |
| `app.rebuild-cache` | throw the cache away and read everything again, after confirmation |
| `app.about` | the about dialog |
| `app.quit` | quit |
| `app.dump-state` | write the current state as JSON (only with `devtools`) |
| `app.snapshot` | write the window as a PNG (only with `devtools`) |

## Running and testing

`just build`, `just run`, `just check`. Tests come in three kinds:

- unit tests in `core`, no display needed,
- widget tests that build the window and drive its actions,
- a smoke test that starts the real binary in a private headless GNOME session, clicks through
  the view switcher and reads the state back.

The last two need a display of their own: `just test-ui` runs the suite inside a headless
session and `just smoke` drives the binary in one. `just fixture` writes a small stand-in
library with the shapes the real one has, and `just ui` opens the application on it.
