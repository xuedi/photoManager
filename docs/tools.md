# Tools

A tool fixes one kind of thing across many photos: missing positions, dates that disagree with
their folder, tags in the wrong shape. The Tools tab is where they live: what they work on, what
each would change right now, and every pass that was ever written, any of which can be taken back.

## What a tool is

A tool is a type in `core` with a key, a title, one line on what it fixes, and one question to
answer: given the cache, the place data, a scope and its settings, what should each photo say? The
answer is a list of wanted changes, and that is all a tool ever produces. Turning it into a
[change set](preview.md), counting it, showing it, applying it, writing it down and taking it back
are shared, so a tool never writes and never draws anything itself. What it may do is ask, in
one shape `core` defines: see [Questions and answers](#questions-and-answers).

The window knows the tools only as a list. It shows each one, counts it and opens it by its key,
and names none of them, so a new tool is one type in `core` and one line in the list.

A tool's settings are a plain value that is written as a line of text and read back from it. A
tool without settings has none to write. That is what lets a suggestion open a tool later with its
scope and its settings filled in: a key, a scope and a line of text, not a form.

A development build lists one more tool, a rating over the whole scope, which exists only so the
way from a tool to a photo can be driven and tested.

## The scope

What the tools work on is one choice at the top of the page: the whole library, a country or an
event from the place tree, or what the gallery handed over with Use as Scope. The dialog lists the
same places the gallery browses, with a search over their names. A place is kept as a filter
rather than a list of paths, so it still means the right photos after a scan
([gallery.md](gallery.md#selection-and-scope)).

## What each tool would change

Every tool in the list says how many photos of the scope it would change. The number is not an
estimate: it is the tool's real change set for the scope, built from the cache off the main thread
through a read-only connection, so the number on the row is the number the preview shows when the
tool is opened.

It is counted again whenever the scope changes and after every scan - and every write is followed
by one - so a count never describes a library that is gone. Only the newest count is shown; one
that arrives late is dropped.

```mermaid
flowchart TD
    scope[the scope] --> count
    list[the tools in core] --> count[each tool's change set for the scope,<br/>off the main thread]
    cache[(cache, read only)] --> count
    count --> rows[the list: what each would change]
    rows -- a tool is opened --> preview[the preview]
    preview -- apply --> engine[write engine]
    engine --> journal[(journal, titled by the tool)]
    engine --> scan[the library is read again]
    scan --> count
    journal --> history[the history]
    history -- take back one pass --> engine
```

Opening a tool builds the same change set again and pushes the [preview](preview.md) on top of
the list. A tool that asks shows its questions first, and its row says what waits for an answer
("8 tags wait for an answer") where a count would say nothing is to be done. A tool may instead
have a page of its own kind: the [tag vocabulary](#tag-vocabulary) opens its tree, and a tool that
needs one line of text, such as [Add a Tag](#add-a-tag), asks for it first. The window chooses the
page by its kind, never by which tool it is.

## Questions and answers

Some things a tool cannot decide from the cache: which place a tag like `places/inGreece/Atens`
means, or whether a camera's clock was off, is the person's answer, not a guess. So a tool may hand
over **questions**, each about a group of photos: a key, a title, what kind of answer it wants, how
many photos wait on it, what is offered for it best first, and the answer so far, and sometimes a
note or the evidence it is decided on. Every offer carries the answer it would give and the words
for it, so **Confirm** puts in whatever the best offer is: a place, a shift, a date. The window
draws the questions on one page without knowing which tool asked or what the groups are; the words
on it - what a question is called, the heading, the bulk button - are the tool's.

The kind of a question decides what its row offers beside Confirm, Leave Alone and Ask Again:

| Kind | Answered with | The row's menu adds |
|------|---------------|---------------------|
| a place | a place or a pin | Choose Another, Pick on Map |
| a camera's clock | a shift per camera | the other offers, Enter a Shift |
| a date | a date, or between the neighbours | the other offers, Enter a Date |
| a time zone | one of the country's zones | the other offers |
| a person | a people tag | the other offers, Enter a Tag |

A tool may also say what it found out about the scope beyond its questions: a few lines drawn
above them, some with a list to open. The page draws them like the questions, without knowing
what they are about.

**Enter a Shift** shows the event's cameras, each with its evidence and an entry, and a camera
left empty is right: `-640d`, `+1y 2d 03:00`. **Enter a Date** takes the one date format. What is
typed is checked before it is kept, and the dialog says what is wrong with it.

A place question is answered with a place, a **pin** or **Leave Alone**. A pin is a point the person
chose on a map, **Pick on Map** in each row's menu: OpenStreetMap tiles, fetched only while the
dialog is open, a click drops the pin and names the town it is in underneath, and **Use This
Point** answers with it. The pin keeps the point and that town, whose name gives the words. Two
pins are the same place only on the same point, and a pin is never the same as a place, even
beside it. A place's position may be 5 000 m off, a pin's 1 000 m: put roughly where it was, not
to the metre.

The answer carries the place itself -
its GeoNames id, name, region, country, code and coordinates - so it does not depend on the place
data it was chosen from, and placing its photos afterwards needs the cache alone. The answers are
the tool's settings: one JSON object, sorted by question, so the same answers are always the same
text, and a suggestion can hand them over like any other settings.

```mermaid
flowchart TD
    open[a tool that asks is opened] --> ask[its questions for the scope,<br/>off the main thread]
    cache[(cache, read only)] --> ask
    geo[(place data, read only)] --> offers[what the place data offers]
    offers --> ask
    ask --> page[the question page]
    page -- Confirm, Choose Another, Pick on Map,<br/>Enter a Shift, Enter a Date, Leave Alone --> answers[the tool's settings]
    page -- the tool's bulk button --> answers
    answers --> kept[(app.db)]
    answers --> page
    page -- Preview --> preview[the change set with the answers so far]
```

An offer may be **sure**. The page's bulk button - worded by the tool, Confirm Exact Matches or
Confirm Where the Rest Is - answers, in one click, every waiting question whose best offer is
sure, and nothing else. The date tools have none: no shift is sure. What counts as sure is the tool's: see each tool below. Everything else is
one click per question, and the preview afterwards still lists every photo. An answer changes the page at once, without
asking the library again; the row on the list is counted again behind it.

## Remembered settings

The last settings a tool was run or answered with are kept in `app.db`, next to the journal, so
they outlive a cache rebuild. Opening a tool without settings uses them, the list counts with
them, and a question answered once is not asked again - not for another scope, not after a new
import. Taking a pass back leaves the answers as they are; running the tool again puts the change
back on.

## GPS from the places tag

The first tool that asks. It gives photos without a position whose places tag names a city the
coordinates of that city, one question per distinct tag rather than one per photo.

- **Who is asked about.** The photos of the scope without GPS, grouped by their deepest places
  tag: `places/inChina/Beijing`, not the `places/inChina` beside it. A photo with a position is
  never in the change set, however it got that position.
- **What the page offers.** The place data's candidates for the tag's last level, with its
  country level as the hint. Sure is an exact name the place data gives 0.9 or more, so the bulk
  button is **Confirm Exact Matches**. A typo, a region or a made-up place still gets its candidates, and
  the search finds the right one or the tag is left alone; no rule for any spelling is written in
  code.
- **A country alone** (`places/inChina`) is listed apart, starts as Leave Alone and is never
  confirmed in bulk: a country centre is nowhere anyone took a photo. It can still be answered by
  hand, for the country that really is one town.
- **What a photo gets.** The city's coordinates, marked in the file as derived (see
  [writing.md](writing.md#the-canonical-field-set)), and the city, region, country and country code in
  words - but the words only when the photo has none, because writing a place takes away every
  part it does not set. A photo with its own words gets the position alone.
- **Two places on one photo** are refused, with both named, unless both tags were answered with
  the same place. A photo whose other tag still waits is held back until it is answered, so it
  cannot end up placed by the first tag before the second could refuse it.
- **Nothing** reaches a photo whose tag is unanswered or left alone.
- **A tag no place data knows** - a village, a made-up place - is answered with a pin.

## GPS from the event

For the photos the places tag cannot place: no GPS and no places tag that names a city - many
carry the country alone. One question per event folder, sub-folders included, answered with a
place, a pin or Leave Alone for an event that was nowhere in particular.

- **Who is asked about.** The photos of the scope without GPS, in an event folder, without a
  city-level places tag. A photo with a city tag belongs to the places tag, whatever that was
  answered, so a trip over two tagged cities never gives one city's photos the other. A loose
  file is not asked about.
- **Where the rest of the event is**, first. The event's photos that have a position - from the
  camera, or given from the places tag - each put in the town it stands in, from the place data.
  One offer per town, "where 3 of its photos are". A small part of a town, a neighbourhood, counts
  as the town unless the position stands on it, so a walk across a city is that city and not a
  dozen neighbourhoods; a district the size of a town is one. The offer's position is the town's,
  not the photos' average.
- **What the folders name**, after that: the city level of the folder if it has one, then the
  event's name, through the place data with the folder's country as the hint.
- **Inside the folder's country.** Every offer is kept to the country the folder names, not just
  weighed towards it. A folder whose country the place data does not know (a part of a country,
  say) is not narrowed, and the row says so.
- **Sure** is only where the rest is, and only when every located photo of the event stands in the
  same town and there are at least three of them. A split event gets an offer per town, none sure.
  A name is never sure, however exact: an event called `Wedding` is not in the Berlin district of
  that name. The bulk button is **Confirm Where the Rest Is**.
- **Nothing from the country alone**: a country centre is not a place anyone took a photo.
- **What a photo gets** is what the places tag gives, with this tool's mark in the file: the
  position, `photoManager: event` and how far off it may be, and the words only where the photo
  has none. A pin is written at its point.

## The offset of a date

A date without an offset is a time on a clock nobody knows. Every date tool writes the offset
along with the date, by one rule, so a photo is written once whichever of them reaches it first.

Where a photo was is its position's nearest place, else the country its folder names
([places.md](places.md#time-zones)). The offset is that zone's at the photo's local time, from the
IANA rules, so a winter photo in Hamburg gets `+01:00` and a summer one `+02:00`, and Beijing is
`+08:00` all year. A position wins over the folder: a photo in a German folder with a position in
Copenhagen is in Copenhagen's zone. The hour summer time skips does not exist and the hour it
repeats happens twice, so a local time in either is refused with that reason and left for the
photo page, never guessed. So is a photo with no position and no country the place data knows, and
every photo when there is no place data at all.

```mermaid
flowchart TD
    photo[a photo with a date] --> pos{position?}
    pos -- yes --> nearest[nearest place's zone]
    pos -- no --> country[folder country's zone]
    country -- several zones --> ask[asked per event]
    nearest --> rules[IANA rules at the local time]
    country --> rules
    ask --> rules
    rules --> offset[offset, or refused<br/>in the summer-time gap or overlap]
    offset --> folder[Dates against the Folder:<br/>shifted date + offset]
    offset --> undated[Photos without a Date:<br/>new date + offset]
    offset --> zones[Time Zones and XMP Dates:<br/>same date + offset]
    folder --> write[one write per photo:<br/>EXIF, offsets, XMP and IPTC agree]
    undated --> write
    zones --> write
```

A photo that has an offset reads as settled to Time Zones and XMP Dates, so a photo the other two
reached is not written again; the list puts them in the order that writes each photo once.

## Dates against the folder

For events whose photos disagree with the date the folder names. One question per event, and the
evidence per camera: how many photos, the first and the last date, and how many days from the
folder. A day either side of a folder's day agrees, as on the [dashboard](dashboard.md); a month or
a year folder agrees with any day inside it.

- **A camera is off** when not one of its photos agrees. A camera with some photos on the folder's
  date and some around it was on a trip longer than the folder says, not wrong, and is offered no
  shift. Photos without a camera name are one "no camera" group.
- **Shift to agree with the others**, where another camera agrees with the folder: the difference
  between the medians of their photos, to the minute.
- **Shift onto the folder day**, where the folder names a whole day: whole days, so the first photo
  lands on it and the time of day stays.
- **The photos are right** is Leave Alone: it writes nothing and is remembered, and fixing the
  folder name is another tool's. It comes first when no camera agrees with the folder.
- **Enter a Shift** for anything else. A shift of more than a year says so on the row.

A shift is per event and camera, never across events: a camera's clock drifts, and the same camera
a year later may be right. It remembers the camera's first date when it was given, so once written
it is never written again, and if the event still disagrees afterwards it is asked about afresh.
The shifted date is written with the offset of where the photo was, or the one it states.

## Photos without a date

One question per event that holds photos without any date.

- **Between its neighbours**: the dated photos of the same folder and the same camera before and
  after it by file name. The photo gets the earlier one's time plus a second per step, so the
  order stays. Offered only between two dated neighbours less than a day apart; a photo at the end
  of its folder, or between two far apart, is refused and left.
- **12:00:00 from the folder**, where the folder names a whole day. A month or a year is no date.
- **Enter a Date** for the rest. A date given to an event goes to its first photo by file name and
  a second more to each one after.

What none of these reach stays for the photo page.

## Time zones and XMP dates

Every photo of the scope with a date and no offset gets the offset of where it was taken, and with
it XMP and IPTC dates that agree with EXIF; the EXIF-shaped XMP dates old writers left, often in
UTC as some other computer saw it, are taken away ([writing.md](writing.md#the-canonical-field-set)).
A photo that states an offset keeps it, so a second run finds nothing to do.

It asks nothing, except where a photo without a position is in a country of several zones: then
one question per event, offered that country's zones. With nothing to ask, its page says so and
Preview still opens.

Over a whole library this is the largest pass there is: nearly every photo, each uploaded once
more, and the preview says how much. Measured over a folder of real photos a write takes around
40 ms, so thousands of photos take minutes; the scope can make it one country or one event at a
time.

## Tag vocabulary

One vocabulary instead of four. The person decides once how the tag tree should look, and every
photo is written so its five tag fields say the same thing ([cache.md](cache.md#tag-fields)).

The decisions are **rules** over tag paths, applied in order:

- **rename A to B** moves `A` and everything below it to `B`. Renaming a leaf, moving a branch to
  another parent and merging into a tag that is already there are all this one rule.
- **delete A** takes `A` and everything below it away.

A photo's deepest tags are followed through every rule, and what comes out is its new tag set. A
root on its own - a photo tagged only `events` - is dropped, since every level is written anyway
and a root alone says nothing; a tag without anything below it anywhere is no root of that kind
and stays. A rule that could never do anything, because an earlier one already moved or deleted its
tag, is refused when it is entered, and so is one that would take a tag back to where an earlier
rule moved it from, or into itself, and one with a separator inside a level or an empty level. The
rules are the tool's settings, so they apply to every later run and every photo imported later:
the vocabulary is decided once.

**The page** is the tree, not a list of questions:

- **Suggestions** on top, from what the tree already shows: two spellings that differ only in
  case are offered to merge into the lower-case one, two sibling tags a letter apart into the more
  used spelling, and each leaf of `mixed` to move to `topics/<leaf>`. Confirm makes it a rule;
  Leave Alone is remembered and not suggested again. The suggestions come from the tree after the
  rules, so a merged pair is gone.
- **The rules** in order, each with how many photos it changes and a button to take it out. A rule
  that only made sense after it goes too, and the page says so.
- **Generated Tags**: what becomes of the tags that only say what the data says - see below.
- **The tree** after the rules, each tag with the photos that carry it or a tag below it, and per
  tag **Rename or Move**, **Merge Into** (a search over the tree) and **Delete**. Deleting a tag
  from more than a hundred photos asks first and says how many. A search turns the tree into a flat
  list of the tags that match.
- **Preview** builds the change set for the scope.

The tree and its counts are worked out off the main thread from the cache, over the whole library
whatever the scope, so a count on the page is the count of the tag, not of the scope.

**Generated tags.** Year, place and event tags repeat what the date, the place words and the
folder already say. By default they are **derived from the data** with every tag write, so they
can never disagree with it: `timeline/<year>` from the date, `places/in<Country>/<City>` from the
place words (the folder's country where the words name none), `events/<year> <name>` from the
event folder. Each replaces the whole branch of its root, so a photo moved to another event gets
the new event on the next run; a photo the data says nothing about keeps what it has, and a photo
without any tag gets them for free. They can also be **dropped**, or **left as they are**, only
mapped by the rules.

```mermaid
flowchart TD
    scan[scan: the tags from every field,<br/>and whether the fields are tidy] --> cache[(cache)]
    rules[the rules, in order<br/>from the tree and the suggestions] --> map
    cache --> map[each photo's deepest tags, mapped]
    map --> generated[generated tags derived, dropped or kept]
    generated --> diff{new set differs,<br/>or the fields are not tidy?}
    diff -- yes --> change[tags in all five fields, label and catalog sets taken away]
    diff -- no --> nothing[not in the change set]
    change --> preview[preview, apply, journal, take back]
```

**Who is in the change set**: every photo of the scope whose tags the rules or the generated tags
change, and every photo whose fields are not tidy, even with its tags unchanged. What is written is
always the whole set: every path with every level in every field, the label and the catalog sets
taken away. A second run finds nothing to do.

**Immich** reads the tags from the files on its next library scan and replaces a photo's tags with
what the file says, so a renamed tag moves there too. A tag no photo carries any more is not
removed by that: it stays in Immich's tag list until its tag cleanup job is started by hand. Tag
names are compared as they are spelled there, which is why the case twins are worth merging.

## Add a tag

One tag onto every photo of the scope, most often a selection handed over from the gallery. It
asks for the tag first, with its levels separated by `/`, and checks it before the preview. What a
photo carries stays; the tag is written with every level into every field, like any tag write. A
photo that carries it already and whose fields are tidy has nothing to do.

## People from Immich

Immich knows who is in the photos: its face recognition found the faces and a person named them.
The files know it only where someone tagged a person by hand, and carry no face region at all. This
tool writes what Immich knows into the files, so the files say it too, and a photo moved or renamed
later loses nothing that only Immich knew. Immich is only read, never written.

- **What it reads.** The snapshot beside the cache, fetched with **Get People from Immich** on the
  dashboard ([cache.md](cache.md#what-immich-knows)): Immich's address is set in Preferences, its
  API key is kept in the GNOME keyring, and Test Connection says whether both work. The key needs
  to read assets, persons, faces and libraries, nothing more.
- **Who is asked about.** Every person Immich has a name for and does not hide, with a face in a
  photo of the scope: one question per person, titled with Immich's name and counted in photos.
  A person without a name, or hidden, is never asked about and never written.
- **What the page offers.** A tag of the people tree with the same name, in either spelling of the
  root, is sure, and **Confirm Exact Matches** takes it; two tags of the same name are both
  offered and neither is sure. Then tags of nearly the same name - the same words in another
  order (family name first or last), one that starts the other, the same first name, a letter or
  two apart - and, where no tag has the name, a new `people/<Name>`.
  **Enter a Tag** takes any tag of the tree, and **Leave Alone** writes nothing for that person.
- **The answers** are kept by Immich's id of the person, not its name, so a person renamed in
  Immich keeps the answer, and so does a new fetch.
- **What a photo gets.** A face region for every answered person Immich found in it, named with the
  last level of the person's tag, so a file names each person one way; the persons named in
  `PersonInImage`; and each person's tag in all five tag fields with every level. What it carried
  stays: a tag is only added. The region list is replaced as a whole, so Immich decides the faces.
- **The boxes.** Immich measures a face on its preview, which is turned the way the photo is shown.
  The file wants the box on the stored picture, before any turn, so every box is turned back by
  the photo's orientation - each of the eight - and clamped to the picture, and the stored size is
  written with it. The way back is tested against Immich's own way there.
- **Who is in the change set.** Every photo of the scope with an answered person whose regions or
  tags do not say it yet. A second run finds nothing; after a new fetch only the photos whose
  people changed in Immich come back.
- **Refused, with why.** A photo Immich has offline; one Immich knows and the library does not; one
  whose stored size or turn is not what Immich saw, or whose faces were measured on a picture of
  another shape, because its boxes would land somewhere else.
- **It runs together** with any tool that does not set the tags, such as the time zones, as one
  write. With the tag vocabulary a photo both would tag is refused: tidy the tags first, then
  write the people.

**Above the questions** the page says what Immich knows that the files do not: when the snapshot
was fetched and how many persons it names, how many photos of the scope do not say someone Immich
found in them, which people tags are carried by photos Immich found no face of that person in, and
how many persons Immich found faces of but has no name for - to be named in Immich, since nothing
is written there from here.

**Immich afterwards.** A write changes a photo's modification time, so Immich reads it again on its
next library scan and runs its face detection once more. It matches the new faces to the old ones
by where they are, and a write that changes only metadata keeps every face on the same person. The
regions written are for every other face-aware viewer: Immich's own face import is to stay off,
or every person would be there twice.

```mermaid
flowchart TD
    snap[(immich.db)] --> ask[one question per named person:<br/>which people tag]
    tree[the people tree, from the cache] --> ask
    ask --> answers[(the tool's settings in app.db)]
    snap --> map[Immich's photos to the library's, by path]
    cache[(cache: size, turn,<br/>regions and tags in the file)] --> map
    answers --> want[per photo: regions, persons, tags]
    map --> want
    want --> diff{the file says it already?}
    diff -- no --> preview[preview, apply, journal, take back]
    diff -- yes --> nothing[not in the change set]
```

## Running tools together

Nextcloud uploads a whole photo again for every edit, so two tools over the whole library are two
uploads of it. **Run Together** on the Tools page chooses several tools and previews them as one
pass, each with the settings and answers it was last given: each photo's changes from every tool
merged into one row, one write, one entry in the journal, taken back as one.

- **One row per photo.** The counts are of photos, so a photo two tools change is counted once.
- **A clash is refused.** Two tools that would set the same field of one photo - both the tags,
  say - refuse that photo with both named.
- **A refusal is one tool's.** A tool that refuses a photo leaves it to the others; only a photo
  every chosen tool refuses is refused, with each reason.
- The pass is named after every tool in it, in the history too.

Time zones and the tag vocabulary are the two passes that reach nearly every photo, and run
together they are one upload instead of two. Measured over two events of real photos, the two
together wrote each photo once, in around 40 ms a photo, a second run found nothing left, and
taking the pass back put every field back exactly.

## The history

Every pass is in the [journal](writing.md#the-journal), named by what ran it: the tool's title, or
the photo that was edited by hand. The history lists them newest first, fifty at a time, each with
when it ran, how many photos it changed, and whether it was taken back. A pass opens to its photos
and what each one got, tag by tag, or why it was left alone. Passes from before passes had names
read as "Earlier change".

## Taking back any pass

Any pass that changed something and was not taken back yet can be taken back - not only the last.
A pass that took something back cannot itself be taken back; running the tool again is how a
change is put back on.

Taking back an older pass is the engine's own undo, and the engine refuses any photo that no longer
says what the pass wrote. So if a later pass changed the same photo again, the later change stays
and that photo is reported as left alone; nothing newer is ever overwritten by something older.
The confirmation says this before anything runs: how many of the pass's photos a later pass
changed again and will therefore be left as they are. A later change that was itself taken back
for that photo does not count, because the photo says again what the older pass wrote.

| A pass | Can be taken back |
|--------|-------------------|
| a write that changed photos, not taken back | yes |
| a write already taken back | no, once is all |
| a write that changed nothing | no, there is nothing to put back |
| a take-back | no, run the tool again instead |

The toast's Undo and the Undo on a photo's own page still mean the last applied change there.
