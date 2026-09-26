# Preview and apply

Nothing is written to a photo that the user has not seen first. A tool works out what it would
change, hands that over as a **change set**, and the change set is what reaches the screen, what
the user trims, and what is handed to the [write engine](writing.md) when they press apply. Every
tool inherits the preview, the traffic estimate and the confirmation by producing one.

The change set is the whole contract between a tool and the writing. No tool talks to the engine.

## A change set

One row per photo: where it is, what its image data was when the change was built, how big the file
is, what would change in words, and a verdict.

| Verdict | Means |
|---------|-------|
| would change | the photo does not say this yet, so it would be written |
| nothing to do | it already says it, as far as the cache knows |
| refused | it will not be touched, and why: not in the cache, unreadable, an intent that cannot be written, or the tool's own reason, such as two places for one photo |
| written / failed | what became of it once the set was applied |

Only a row that would change can be selected, and only selected rows are ever handed to the engine.

A change set can instead be one of **moves**: one row per folder or photo that would go somewhere
else, with where it goes and how many photos go with it, and the same verdicts. Such a set is built
from the cache like any other, and then the folders it would move are looked at on disk - never a
photo in them: a target that is there already, or a folder that holds a file the scan does not know
or lacks one it does, is refused in the preview as the engine would refuse it on apply. A set is
either moves or writes, never both.

## Why the preview reads the cache and the apply reads the file

A pass over the whole library is one row per photo, and re-reading every photo with ExifTool to
build the preview would take a quarter of an hour. So a change set is built from the
[cache](cache.md) alone: a database query and some arithmetic, over thousands of photos in
milliseconds. Building one opens no photo and writes nothing at all.

That is safe because the safety does not depend on it. The engine re-reads and re-proves every
photo at apply time regardless - that is its job - so a row that has gone stale becomes a harmless
`skipped`, or a `refused` when the image data moved under us, and never a wrong write.

```mermaid
flowchart TD
    tool[a tool decides] --> set[change set]
    cache[(cache)] -- what each photo says --> set
    set --> table[the table: one row per photo]
    table -- a row is asked about --> dry[dry run of that one photo]
    photo[(the photo)] -- one read --> dry
    table -- rows dropped --> picked[what is selected]
    picked --> apply{apply}
    apply --> engine[write engine]
    engine --> photos[(the photos)]
```

The cache does not keep everything a change can set. Where it cannot answer - a place in words or
a label being cleared - the row says so rather than guessing, and the verdict errs
towards "would change": over-estimating costs an estimate, under-estimating would silently drop a
photo the user wanted changed.

## Two levels of detail

A row says the change the way a person thinks about it - `location: none -> 39.90420, 116.40740` -
because nobody reads ten IPTC and XMP tags per photo across thousands of rows.

Asking about one row runs a **dry run** of that photo: the engine's own code path up to, and not
including, the copy it writes. It costs one ExifTool read and returns every tag that write would set with
the value the tag has now. It is asked once per row and kept. The human summary is for scanning,
the exact detail is for checking, and the second is never a guess.

## What a pass costs

Nextcloud re-uploads the whole file for every edit, so the estimate is the sum of the file sizes of
the selected rows that would actually change - not a guess at the size of the difference. It is
shown in the summary and next to the apply button, so a pass over the library is a decision and not
a surprise. A move uploads nothing: Nextcloud takes a renamed folder for a move on the server, so a
set of moves costs no traffic.

## Before the first ever write

We cannot know that a backup exists, so the application asks instead of pretending to check. Before
anything is written for the first time a dialog says plainly what is about to happen, that
photoManager cannot take a change back and the backup is the way back, and requires an explicit
acknowledgement. It is recorded once in `app.db`, so it survives a cache rebuild and is never asked
again.

If the user names a backup location it is looked at: is it there, is anything in it, when was it
last changed. That is help for a person reading the dialog, and it is said in as many words that it
is not proof that their photos are in it.

## Applying

The selected rows are written as one pass, one photo after another, off the main thread. Progress
is shown in place, cancel stops between photos, and a toast says what happened. Every row then carries its outcome, so the table shows what became of
each photo instead of what would have.

A written photo is forgotten by the cache - metadata changed, so what was remembered about it is
stale - and the library is read again afterwards. Reading is all that is: the photos are not
touched by it.

## One photo

A photo edited by hand on its [page](photo.md) is a change set of one row, built the same way from
the cache. It needs no table, so it is reviewed in a dialog instead: the exact assignments of that
one photo, from the same dry run a row's detail uses, and the size of the file that goes up again.
Apply writes it through the same backup question and engine; the two share the questions, so a
person is asked the same thing in the same words wherever the write comes from.

## On screen

The tools view is a navigation stack. Its root is the scope and the list of [tools](tools.md);
a tool's form, once filled in, builds its change set for the scope and pushes the preview on top
of it, and the back button returns. A preview only means anything while a tool run is in flight,
so it is not a view of its own.

The [suggestions](suggestions.md) need no table: each fix already says what it changes, and the
tick is the confirmation. Their passes go through the same backup question and engine.

On a narrow window the table keeps columns wide enough to read and scrolls sideways inside
itself, so the preview never makes the window wider than a phone.

The table is virtualised - a column view over a list model - because at one row per photo of the
whole library a widget per row would take seconds to build and hundreds of megabytes.
