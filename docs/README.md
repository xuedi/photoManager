# Documentation

How the parts of photoManager work. One file per subsystem, added when the subsystem exists.

| Document | Subsystem |
|----------|-----------|
| [overview.md](overview.md) | the two crates, where data lives, actions, how it is run and tested |
| [cache.md](cache.md) | what the application remembers about the photos, how a scan fills it and follows a renamed file, what an issue is, the place check, the face regions and the persons, what Immich knows |
| [thumbnails.md](thumbnails.md) | the small pictures a grid draws, keyed by the image rather than the path |
| [places.md](places.md) | turning place names into coordinates and back, without the network |
| [writing.md](writing.md) | the only part that changes a photo: how one write works, what is proved, a photo written anyway, moving a folder, why there is no undo |
| [preview.md](preview.md) | what a tool would change, on screen: the change set, the estimate, the confirmation |
| [tools.md](tools.md) | the edits the person drives: what a tool is, the scope, Set Place, the offset of a date, Shift Dates, Set Date, Set Time Zone, the tags and Tag to Person, Places Tag to Sublocation, Move Event |
| [suggestions.md](suggestions.md) | the fixes the app is sure of, ticked and applied finder by finder: the tag tree, people from Immich and from tags, places from tags and from events, place words from GPS, folders, file names |
| [dashboard.md](dashboard.md) | the upkeep bar and when each job is worth running again, what the library is missing and where, what is left for GPS, where the places tags disagree, the filters that name a set of photos, and the way from each finding to its fix |
| [gallery.md](gallery.md) | finding photos by place, tag, person and gap, the active filter as chips, counts that follow the filter, the grid and its pictures, the scope a tool is handed |
| [photo.md](photo.md) | one photo: the full size, stepping, the panel, the map, and editing one photo safely |
