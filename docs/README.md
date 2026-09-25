# Documentation

How the parts of photoManager work. One file per subsystem, added when the subsystem exists.

| Document | Subsystem |
|----------|-----------|
| [overview.md](overview.md) | the two crates, where data lives, actions, how it is run and tested |
| [cache.md](cache.md) | what the application remembers about the photos, how a scan fills it, what an issue is, the face regions, what Immich knows |
| [thumbnails.md](thumbnails.md) | the small pictures a grid draws, keyed by the image rather than the path |
| [places.md](places.md) | turning place names into coordinates and back, without the network |
| [writing.md](writing.md) | the only part that changes a photo: how one write works, what is proved, how it is taken back |
| [preview.md](preview.md) | what a tool would change, on screen: the change set, the estimate, the confirmation, the undo |
| [tools.md](tools.md) | what a tool is, the scope, what each would change, questions and answers, the pin, GPS from the places tag, GPS from the event, the offset of a date, the three date tools, the tag vocabulary, adding a tag, people from Immich, running tools together, the history and taking any pass back |
| [dashboard.md](dashboard.md) | what the library is missing and where, and the filters that name a set of photos |
| [gallery.md](gallery.md) | finding photos by place, tag and gap, the grid and its pictures, the scope a tool is handed |
| [photo.md](photo.md) | one photo: the full size, stepping, the panel, the map, and editing one photo safely |
