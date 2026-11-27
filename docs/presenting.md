# Presenting

`luchctl present [receiver] <file>`, or *Present…* in the GNOME menu, shows a PDF or a presentation on the TV and
opens a presenter view on this computer.

- **On the TV**: the slide, drawn by the receiver from the PDF at the TV's own resolution (PROTOCOL.md 4g). The laser
  pointer and the ink are drawn by the receiver too, over the slide (4h): sharp on a 4K screen, and nothing waits
  for a video encoder.
- **Here**: the current and the next slide, the speaker notes, the elapsed time (T pauses, R resets), the clock and the
  slide counter. Served by `luchd` on `127.0.0.1` only, under a random token, to a Host of this machine only.

## Driving it

| Input | Does |
|---|---|
| Mouse over the slide | moves the laser pointer on the TV; leaving the slide hides it (L turns the laser off) |
| Drag on the slide | draws in the colour chosen; the marker is wide and translucent |
| Click / right click on the slide | next / previous slide |
| PageDown, →, ↓, Space, Enter, N | next slide |
| PageUp, ←, ↑, Backspace, P | previous slide |
| Home / End | first / last slide |
| B or `.` | black screen on the TV (again: back) |
| F5, Escape | back from black; a clicker's start button sends these in turn, so it never leaves the presentation |
| Ctrl+Z, C | undo the last stroke, clear the slide's ink |
| + / − | bigger or smaller notes |

**Clickers.** Presentation remotes are keyboards: PageUp/PageDown, B or `.`, F5/Escape. Plugged into this computer,
they drive the presenter view (keep its window in front). Plugged into the TV, the receiver obeys the same keys
itself: PageUp/PageDown turn the page, B and `.` black the screen, F5 and Escape bring it back. The TV's own remote
turns pages with left/right.

Ink stays with its slide: going back shows it again. Pointer moves are sent at most once every 16 ms (the page
merges them, and so does `luchd`), always the newest position; strokes go out every 30 ms while they are drawn.

## Speaker notes

| File | Where the notes come from |
|---|---|
| `.odp` | the notes of each slide in `content.xml`, read directly |
| `.pptx` | the notes slides (`ppt/notesSlides/`), in the order of `presentation.xml`, read directly |
| `.pdf` | `talk.pdfpc` next to `talk.pdf` (pdfpc's JSON or older text form, as Beamer's `pdfpc` package writes), else the PDF's text annotations ("sticky notes") on each page |

LibreOffice makes the PDF the TV shows, and drops the notes doing so; exporting notes pages and reading their text
back out of a PDF would be slower and mix the slide's own text in. So the notes are read from the original file:
both formats are zip archives of XML with the notes as plain text. Hidden slides are skipped, as LibreOffice leaves
them out of the PDF, so the notes stay on the right page.

Slide pictures in the presenter view need `pdftoppm` (poppler-utils) or `mutool` (mupdf-tools); without them the
view still turns pages, shows the notes and the timer.

## Do not disturb

`luchctl quiet ask|on|off` (D-Bus property `Quiet`; the GNOME menu cycles it). While presenting or mirroring, `on`
turns GNOME's notification banners off and puts them back afterwards; the person's own "off" is left alone. `ask`
(the default): the GNOME extension asks once a login. What was changed is written to
`~/.config/luch/restore-banners` first, so a crash is undone at the next `luchd` start, as with the sound.

## The cursor as metadata (plan, not built)

Mirroring now burns the cursor into the frames (`cursor-mode` embedded). The screencast can instead deliver it as
metadata (`cursor-mode` 2, `SPA_META_Cursor`: position, hotspot, and a bitmap when the shape changes), and the
receiver could draw it, or a laser dot in its place.

What it would bring:

- A sharp cursor at the TV's resolution, not scaled and compressed with the picture.
- No frame to encode for a cursor moving over a still screen: on a slide or a document that is most frames, so less
  bandwidth and less encoder heat. With damage-driven capture, frames are made only when the content changes.
- The cursor could become a laser pointer while mirroring a presentation app.

What it costs:

- **GStreamer's `pipewiresrc` drops the metadata**: it negotiates no `SPA_META_Cursor`, and there is no element
  that passes it on (PipeWire issues 120 and 3853). It needs a PipeWire consumer of our own (pipewire-rs) next to,
  or instead of, `pipewiresrc`: the larger part of the work.
- The `fast` mode (KMS scanout through `luch-kms`) does not use PipeWire: the cursor plane would have to be read
  from KMS (`drmModeGetPlane` on the cursor plane, its framebuffer for the shape).
- The cursor would run ahead of the picture by the video latency (30–60 ms): a dragged window lags behind the
  cursor that drags it. Fine for pointing, odd for dragging.
- Niri's virtual outputs (`RecordMonitor` with `cursor-mode`) support it; the portal path needs
  `AvailableCursorModes` checked.

Plan, if wanted: (1) `pointer` gains `"style":"cursor"` and a `cursor` message `{"t":"cursor","png":"<base64>",
"hot":[x,y],"id":n}` sent once per shape (a 64×64 PNG fits a control line); the receiver draws the shape at the
pointer position, scaled from the stream size. (2) luchd: a pipewire-rs stream for `monitor`/`window`/`virtual` that
reads `spa_meta_cursor` and hands frames on as `pipewiresrc` does now, behind an option; the pointer goes through the
same coalescing as the presenter view's laser. (3) `fast`: read the KMS cursor plane. Until then the GNOME extension's
cursor overlay (`cursor.js`) covers the virtual monitor, and `Annotate` gives any tool a receiver-drawn pointer.
