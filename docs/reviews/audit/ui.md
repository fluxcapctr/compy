# UI audit: window, canvas, tools, layers, dialogs, theme, CLI

Scope: `src/ui/*` and `src/main.rs`, whole-project pass after `REVIEW_RESULTS.md` .. `REVIEW_RESULTS_8.md`.
Nothing already listed in those eight reports is repeated here. Report only; no file was edited and
`cargo` was not run.

Counts: 2 P1, 7 P2, 16 P3.

---

## P1

### 1. [P1] A transform, artboard or pixel drag panics when its layer disappears mid-drag

`src/render/mod.rs:194`

```rust
pub fn layer(&self, id: Uuid) -> &Layer { &self.layers[self.index[&id]] }
```

`HashMap`'s `Index` panics on a missing key. Three drag paths keep layer ids captured at press time and
re-read them on every pointer step without revalidating:

- `src/ui/canvas.rs:975` — `let moved = if originals.len() == 1 && !d.document.renderer.layer(*id).is_group()`
- `src/ui/canvas.rs:1009` — `originals.iter().map(|(id, _)| (*id, d.document.renderer.layer(*id).transform))`
- `src/ui/canvas.rs:857-863` → `Document::nudge_artboard` (`src/document.rs`), `self.renderer.layer(id).artboard`

The keyboard stays live during a drag: `src/ui/mod.rs:408` lets Ctrl/Alt combinations through to the GTK
accelerators, and `src/ui/mod.rs:412-415` routes bare keys into `Canvas::special_key`, whose Delete arm
(`src/ui/canvas.rs:806-818`) calls `Document::delete_layer` with no `busy_editing`/drag guard.
`delete_layer` (`src/document.rs:2850`) removes the layer from the renderer immediately.

Exact sequences:

1. Move tool (V). Press and hold button 1 on a layer with no selection active, drag a few pixels (do not
   release). Press `Delete`. Move the mouse one more pixel.
   → `update_transform` → `src/ui/canvas.rs:975` → `self.index[&id]` panic. If you release instead of
   moving, `finish_transform` → `src/ui/canvas.rs:1009` panics the same way.
2. Same, but press `Ctrl+Z` instead of Delete, with the previous history step being one that created the
   layer (New Layer, Paste, Import, Duplicate, Layer via Copy). `win.undo` fires (`src/ui/mod.rs:473`),
   `Document::apply` restores a state without that layer, and the next motion or the mouse-up panics.
3. Move tool, press on an artboard name strip and drag it (`begin_board_drag`, `src/ui/canvas.rs:841`),
   press `Delete` (the board is the active layer, so it is what goes), move the mouse →
   `nudge_artboard` → `self.renderer.layer(id).artboard` panic.
4. The same window exists for the assistant: a `layers` tool call that deletes or merges during a drag
   leaves `transform_drag`'s `originals` stale.

Nothing rechecks `has_layer` between press and release. `Document::apply` does keep `active` valid, so
`geometry()` and `draw_overlays` are safe; only the drag-captured ids are not.

Confidence: confirmed by tracing.

### 2. [P1] Switching tools during a mask stroke silently discards the whole stroke and records an empty undo step

`src/ui/canvas.rs:590` (`tool_changed`), `src/render/mod.rs:421` (`end_mask_preview`),
`src/document.rs` `finish_stroke_named`.

`tool_changed` runs on every tool change and unconditionally tears the stroke preview down:

```rust
if d.tool != Tool::Gradient { if let Some(id) = d.document.active { d.document.renderer.set_preview(id, None); d.document.renderer.end_mask_preview(id); } }
```

It does not clear `self.painting`, so the stroke keeps running. For a mask stroke the preview surface *is*
the stroke's destination: after `end_mask_preview`, `sync_mask_preview` bails at
`let Some(a8) = self.renderer.mask_preview(id) else { return Ok(()) }` and `adopt_mask_preview`
(`src/render/mod.rs:429`) returns `false` because `self.mask_previews.remove(&id)` finds nothing. But
`finish_stroke_named` takes the success branch anyway:

```rust
if result.is_ok() && stroke.touched_anything() {
    self.begin_edit("Paint Mask");
    self.renderer.adopt_mask_preview(id);   // no-op
    self.end_edit();
}
```

Sequence: add a layer mask, click the mask thumbnail in the layers panel so the mask is the target, pick
the Brush (B), press and drag across the canvas, and while still holding the button press any tool key
(`v`, `e`, `m` …). Release.
→ The painted mask coverage is gone, the canvas preview vanished halfway through the drag, and the
history gains a "Paint Mask" step that changes nothing (so Undo appears to do nothing).

The pixel path is luckier: `finish_stroke_named` builds the new image from `stroke.preview` and calls
`renderer.set_image`, so pixels survive; only the live preview disappears for the rest of the drag. The
mask path is the one that loses work.

Confidence: confirmed by tracing.

---

## P2

### 3. [P2] View > New Guide opens nothing

`src/ui/dialogs.rs:443-450`

```rust
pub fn new_guide(parent: &gtk::Window, done: impl Fn(bool, f64) + 'static) {
    let (window, grid, ok) = dialog(parent, "New Guide");
    ...
    ok.connect_clicked(move |_| { done(orientation.selected() == 0, position.value()); window.close(); });
}
```

`window` is moved into the OK handler and `window.present()` is never called. It is the only function in
`dialogs.rs` that omits it (every other one presents at lines 57, 93, 121, 130, 145, 157, 171, 248, 298,
326, 336, 345, 356, 392). `dialogs::floating` also does `window.set_application(...)`, so the invisible
window stays registered with the `GtkApplication` for the rest of the session and would be picked up by
`snapshot_window`'s `app.windows()` loop in screenshot mode.

Sequence: menu bar > View > New Guide… → nothing happens, no error, no window.

Confidence: confirmed by tracing.

### 4. [P2] A Gradient or Shape drag still commits after the tool is switched mid-drag

`src/ui/canvas.rs:587-597` (`tool_changed`), `src/ui/canvas.rs:549-553` (drag update),
`src/ui/canvas.rs:1189-1218` (`finish_tool_drag`).

`tool_changed` clears `handles_parked`, `pen_drag`, `draft`, `d.crop`, `d.gradient_line` and
`d.shape_draft` — but not `self.tool_drag`, `self.painting`, `self.transform_drag`, `self.board_drag`,
`self.guide_drag`, `self.outline_move` or `self.pixel_moving`. The drag-update chain
(`src/ui/canvas.rs:527-569`) dispatches purely on those cells, never on `d.tool`, and so does
`finish_tool_drag`, which matches on the stored `ToolDrag` variant.

Sequence: Gradient tool (G). Press button 1 on the canvas and drag (do not release). Press `v`.
→ `tool_changed` wipes `gradient_line` and the preview; the next pointer step runs
`update_tool_drag`'s `ToolDrag::Gradient` arm, which sets `gradient_line` again and re-previews; on
release `finish_tool_drag` calls `gradient_fill(..., true)` and a gradient is committed as an undo step
while the rail and options bar show the Move tool.

Same with Shape (U) → `add_shape_layer` creates a shape layer after the switch. With Crop (C) the frame is
recreated behind the user's back and stays in `d.crop` invisibly until the Crop tool comes back and
`Return` applies it.

Confidence: confirmed by tracing.

### 5. [P2] Closing a tab leaks the whole document and leaves a 120 ms timer running forever

`src/ui/canvas.rs:111-113`, `152`, `219`, `269-279`, `282`, `302`, `324`, `341`, `423`, `490`.

`Canvas` owns `pub area: gtk::DrawingArea` and then hands strong `Rc<Canvas>` clones to callbacks
installed *on that same widget*:

```rust
let (draft_ref, outline_ref, this) = (self.clone(), self.clone(), self.clone());
area.set_draw_func(move |area, cr, w, h| { ... });          // canvas.rs:219-264
```

plus the motion, scroll, pinch, right-click, click and drag controllers (`282`, `302`, `324`, `341`, `423`,
`490`). That is `Rc<Canvas> → Canvas.area (GObject ref) → closure → Rc<Canvas>`: a cycle. `Rc::new_cyclic`
at `canvas.rs:146` shows the author was aware of the problem (the `ToolRail` callback uses a `Weak`), but
`connect()` does not.

Consequence: `impl Drop for Canvas` (`canvas.rs:111-113`), whose only job is
`if let Some(id) = self.ants.take() { id.remove(); }`, never runs. After
`close_page` → `notebook.remove_page` → `prune()` drops the `Page`, the `Canvas`, its `DocRef` and the
whole `Document` (layer surfaces, halvings, thumbnails, and up to the 256 MB history budget set in
`Document::new`) stay alive, and the marching-ants timeout at `canvas.rs:269` keeps firing every 120 ms
for the rest of the session, calling `doc.try_borrow_mut()` and `area.queue_draw()` on a destroyed widget.
One more orphan timer per tab closed.

Sequence: open a 50 megapixel `.comp`, paint on it, Ctrl+W. RSS does not fall; `COMPOSITOR_TRACE` shows
nothing, but the timer count grows with each open/close cycle.

The gradient editor has the same shape: `Editor` owns `bar`, `colors`, `alphas` and those areas' draw
funcs hold `Rc<Editor>` (`src/ui/gradient_editor.rs:69-71`), so every Gradient Editor session leaks an
`Editor` and with it the `changed` closure's captured `DocRef`.

Confidence: confirmed by tracing.

### 6. [P2] Space held while focus leaves the window leaves the canvas stuck in pan mode

`src/ui/mod.rs:404-407`, `423-425`; `src/ui/canvas.rs:426`, `497`, `605`.

```rust
if key == gdk::Key::space { if !state.space_held.get() { state.space_held.set(true); ... } return Stop; }
...
keys.connect_key_released(move |_, key, _, _| { if key == gdk::Key::space { released.space_held.set(false); ... } });
```

`space_held` is cleared only by a key-release event on the main window. There is no focus-out,
`is-active` or `notify::has-focus` reset anywhere (grep over `src/ui/` finds only the two handlers and the
three readers).

Sequence: hold Space over the canvas (cursor becomes "grab"), Alt+Tab to another window, release Space
there, Alt+Tab back.
→ `space_held` stays `true`. `Canvas::update_cursor` (`canvas.rs:605`) keeps reporting "grab",
`click.connect_pressed` returns immediately at `canvas.rs:426` so no tool click registers, and every
button-1 drag pans (`canvas.rs:497`). The only way out is to press and release Space again over the
window.

Confidence: confirmed by tracing.

### 7. [P2] `needs_redraw` is written and never read: options-bar edits do not repaint the canvas

`src/ui/mod.rs:115` (field), `src/ui/tools.rs:218` and `src/ui/tools.rs:580` (the only writers).

```rust
d.document.set_transform(id, t, "Transform Layer");
d.needs_redraw = true;                      // tools.rs:580
```

Nothing anywhere reads `Doc::needs_redraw`, and neither `connect_move_fields` nor `TypePage::connect`
calls `queue_draw` or the page refresh. The canvas overlay is a separate `GtkDrawingArea` over a
`GtkPicture`; GTK4 will not re-run its draw func because an unrelated spin button changed.

Sequences:

- Move tool, click into the X field of the options bar, type `400`, then press `Tab` or click another
  field. The layer moves in the document but the canvas keeps showing it at the old place. (Pressing
  `Return` instead happens to work, because `src/ui/mod.rs:394-397` schedules `park_handles()`, which does
  call `queue_draw`.)
- Type tool with a type layer selected: click the Size spin's up arrow, or change Leading, Tracking, Bold
  or the family. `set_text` runs, `needs_redraw` is set, the canvas does not repaint. README claims
  "changes apply to the selected type layer live".

Any unrelated redraw (moving the pointer with a brush tool, the marching-ants tick when a selection
exists, scrolling) hides the bug, which is probably why it survived.

Confidence: confirmed by tracing.

### 8. [P2] The filter dialog follows the active layer, so it previews and commits on the wrong one and strands a preview on the first

`src/ui/filter_dialog.rs:103` (`floating(parent, &title, false, 360, &content)` — not modal),
`src/ui/filter_dialog.rs:714-716`, `741-745`; `Document::preview_filter` and `Document::clear_preview`
both key off `self.active`.

```rust
pub fn clear_preview(&mut self) {
    if let Some(id) = self.active { self.renderer.set_preview(id, None); self.renderer.end_mask_preview(id); }
}
```

Sequence: select layer A, Filter > Blur > Gaussian Blur…, drag the Radius slider (A now carries a preview
surface). The window is non-modal, so click layer B in the layers panel. Drag the slider again.
→ `render_preview` → `preview_filter` now previews on B. A's preview is never cleared. Press Cancel →
`clear_preview` clears B only, so **A keeps a blurred preview on screen indefinitely** while
`is_modified()` still reports the document unchanged, so what the canvas shows and what `save` writes
disagree. Press OK instead and `apply_filter` commits to B, though the dialog's title, histogram
(`filter_dialog.rs:35`) and the "select an image layer first" check in `App::open_filter`
(`src/ui/mod.rs:1317`) all referred to A.

The same applies to clicking a mask thumbnail while the dialog is open: `preview_filter` switches to the
`active_mask()` branch mid-session and the layer keeps its pixel preview.

Confidence: confirmed by tracing.

### 9. [P2] Opening the same project twice gives two tabs sharing one `document_id`, so they fight over one autosave

`src/ui/mod.rs:1036-1052` (`open_path` always calls `add_page`, no dedup),
`src/ui/mod.rs:1102`/`1107` (`autosave::discard(id)`), `src/ui/mod.rs:1369`,
`src/document.rs:112` (`let document_id = project.manifest.document_id;`),
`src/autosave.rs:69` (`path_for(id) = dir().join(format!("{}.comp", upper(id)))`).

A `.comp`'s `document_id` comes from its manifest, so two tabs opened from the same package (or from a
`cp -r` copy of it) have the same id, and `autosave::path_for` gives them the same recovery file.

Sequences:

- Ctrl+O the same project twice. Edit both. The 2-minute autosave timer (`App::autosave_all`,
  `src/ui/mod.rs:1163-1181`) writes both documents to the same `~/.local/share/compositor/autosave/<ID>.comp`,
  each overwriting the other. After a crash only one tab's work is recoverable, and which one is a race.
- Open it twice, edit tab B, close tab A without changes. `close_page`'s unmodified branch runs
  `crate::autosave::discard(id)` (`src/ui/mod.rs:1102`) and deletes tab B's recovery copy.
- Save in either tab: `save_to` (`src/ui/mod.rs:1369`) discards the shared autosave as well.
- Same collision between a recovered document and its original: `open_path` (`src/ui/mod.rs:1039-1044`)
  clears the title and path for a recovered file but leaves `document_id` as it was.

Confidence: confirmed by tracing.

---

## P3

### 10. [P3] `Layer > Edit Shape Points` leaves the options bar on the previous tool

`src/ui/canvas.rs:1322-1323`, `src/ui/tools.rs:63-67`.

`shape_edit` writes `d.tool = Tool::Pen` directly and *then* calls `self.set_tool(Tool::Pen)`. The rail's
toggle handler short-circuits on the already-set tool:

```rust
button.connect_toggled(move |b| {
    if !b.is_active() { return; }
    if let Ok(mut d) = doc.try_borrow_mut() { if d.tool == tool { return; } d.tool = tool; }
    changed();
});
```

so `changed()` → `finish_text_edit()` + `tool_changed()` never runs: `options.update(Pen)` is skipped and
the stale `draft` / `handles_parked` state is not cleared. Sequence: Move tool, select a shape layer made
from a path, Layer > Shape > Edit Shape Points. The rail shows Pen selected, the path draws, but the
options bar still shows the Move inspector (X/Y/W/H/Angle) instead of the Pen buttons.

Same handler has a second hole: when `try_borrow_mut` fails, `d.tool` is left unchanged but `changed()`
still runs, so the rail and the document disagree about the current tool.

Confidence: confirmed by tracing.

### 11. [P3] The shortcuts table advertises Shift+M and Shift+L, which do nothing

`src/ui/mod.rs:971` claims `Shift+M, Shift+L, Shift+U` "Swap the marquee, lasso or shape kind", and the
README repeats it. Only `'U'` is handled (`src/ui/canvas.rs:1671`):

```rust
'U' if d.tool == Tool::Shape => { d.shape_ellipse = !d.shape_ellipse; ... }
```

`'M'` and `'L'` fall through `brush_key` and reach the tool lookup at `src/ui/mod.rs:417`, which lowercases
the character and simply re-selects the Marquee or Lasso. `marquee_ellipse` / `lasso_polygonal` are only
reachable from the options-bar dropdowns (`src/ui/tools.rs:327`, `337`).

Confidence: confirmed by tracing.

### 12. [P3] `compositor render` writes the PNG and *then* rejects an extensionless output

`src/main.rs:151-154`

```rust
png_io::encode(&rendered.image, output, rendered.resolution)...?;
println!(...);
if output.extension().is_none() { bail!("output has no extension"); }
```

Sequence: `compositor render project.comp out` → `out` is written, a success line is printed, and then
`error: output has no extension` goes to stderr with exit 1. A script that checks the exit code deletes or
retries, while a file it did not want already exists.

Confidence: confirmed by tracing.

### 13. [P3] A subcommand with too few arguments silently launches the GUI with the subcommand name as a file path

`src/main.rs:13-59`. Every subcommand arm is guarded by an `args.len()` condition, and the fallback arm at
line 58 only catches `info`, `render`, `psd`, `--help` and `-h`. Everything else drops into the GUI arm
at line 59, whose `_ => paths.push(PathBuf::from(arg))` treats the subcommand name as a path.

Sequences: `compositor tool`, `compositor patterns`, `compositor brushes`, `compositor convert-brushes out.abr`
→ the window opens and reports "Could not open … tool", instead of printing `USAGE`. Non-interactive
callers get a GUI process rather than an exit code.

Confidence: confirmed by tracing.

### 14. [P3] `patterns import` and `convert-brushes` exit 0 when every input fails

`src/main.rs:33-53`, `23-32`. Both loop over the inputs, print `skipped <path>: <error>` for each failure,
and return `Ok(())` regardless.

Sequence: `compositor patterns import /does/not/exist.abr; echo $?` → prints "skipped …",
"imported 0 patterns", exit 0. `convert-brushes` goes further and writes an ABR containing zero tips.

Confidence: confirmed by tracing.

### 15. [P3] `--filter` cannot name three kinds the menu has, and ignores typos in silence

`src/main.rs:98-105` vs `src/ui/mod.rs:571-576`. The menu action accepts `hsv`, `exposure` and `fade`;
the CLI table does not. `text().and_then(|f| match f.as_str() { ... _ => None })` turns both an unknown
name and a missing kind into `script.filter = None`, and `App::run` then just does nothing.

Sequence: `compositor a.comp --screenshot out.png --filter hsv` → the app opens, no dialog, the screenshot
is taken, exit 0. Same for a typo like `--filter guassian`. Related: `--tool` matches the *variant* name
(`src/main.rs:70`, `format!("{tool:?}").to_lowercase()`), so the Smear tool is `--tool blur` and Spot
Healing is `--tool heal`, neither of which is the name shown in the UI.

Confidence: confirmed by tracing.

### 16. [P3] Save As eats the last dot-separated segment of a typed name

`src/ui/dialogs.rs:403-411`

```rust
if path.extension().is_none_or(|e| e != extension) { path.set_extension(extension); }
```

`PathBuf::set_extension` replaces everything after the last dot. Sequence: File > Save As, type
`Poster v1.2` → saved as `Poster v1.comp`. Export PNG with `logo.dark` → `logo.png`. The user is never
told the name changed; only the "Saved <name>" status line shows it, in the canvas status bar.

Confidence: confirmed by tracing.

### 17. [P3] A layer smaller than about 20 view points cannot be moved, only resized

`src/transform.rs:108-121`

```rust
let near = |o: (f64, f64)| (point.0 - o.0).hypot(point.1 - o.1) <= 10.0;
if near(self.rotation) { return Some(Mode::Rotate); }
if let Some(i) = self.handles.iter().position(|h| near(*h)) { return Some(Mode::Resize(i)); }
```

The tolerance is a constant in view points while the handle positions are in view space, so at low zoom
all eight handles fall inside one another's radius and `position` always returns the first match, index 0
(top-left corner).

Sequence: a 100 × 100 px logo layer on a 6000 px canvas at fit (about 10 %). The box is ~10 view points
across. Move tool, press anywhere on the layer → `begin_transform` gets `Resize(0)` and the drag scales
from the top-left corner; the cursor shows `nwse-resize`. Dragging the layer is impossible until you zoom
in. (The rotation handle is a fixed 28-point offset from the top-center handle, `src/transform.rs:104`, so
it does not collapse in; the eight box handles do.)

Confidence: confirmed by tracing.

### 18. [P3] Tab always hides the panels, so keyboard focus can never leave a non-entry control

`src/ui/mod.rs:392-399`, `409`.

The capture-phase handler lets keys through only when the focused widget is a `gtk::Editable` or
`gtk::Text`; everything else falls to line 409:

```rust
if key == gdk::Key::Tab && !modifiers.contains(SHIFT_MASK) && state.notebook.current_page().is_some() { state.toggle_panels(); return Stop; }
```

Sequence: click the "Lock ratio" check box, or the blend-mode dropdown, or the Flip H button, then press
Tab. Instead of moving focus to the next control, the whole tool rail, options bar and layers panel
disappear. Only spin buttons and entries let Tab through. Photoshop's Tab behaviour is intended, but the
Editable-only exemption makes the rest of the chrome unnavigable by keyboard.

Confidence: confirmed by tracing.

### 19. [P3] Escape closes only two of the eight floating windows

`src/ui/effects.rs:82-84` and `src/ui/mod.rs:1243-1245` install an Escape key controller. Nothing
equivalent exists in `dialogs::dialog()` (`src/ui/dialogs.rs:22-36`), `dialogs::export_sizes`,
`dialogs::history`, `filter_dialog::present` or `gradient_editor::open`. These are plain `gtk::Window`s
built by `dialogs::floating`, not `GtkDialog`s, so GTK provides no default Escape handling either.

Sequence: Image > Canvas Size…, press Escape → nothing. Same for New Canvas, Image Size, Feather, Stroke,
Color Range, Fill with Pattern, New Artboard, Rotate Canvas, Rename, Export JPEG, Export Sizes, History,
any filter dialog, and the Gradient Editor. Return does work everywhere (`window.set_default_widget(&ok)`).

Confidence: confirmed by tracing.

### 20. [P3] Every panel rebuild scrolls the layer list back to the top and throws away an in-progress rename

`src/ui/layers.rs:176-199`, `426-430`.

```rust
while let Some(child) = self.list.first_child() { self.list.remove(&child); }
```

`rebuild()` destroys and recreates every row, and `list.select_row` at line 428 does not scroll the
selection back into view. `Page::refresh` (`src/ui/mod.rs:144-148`) calls `panel.rebuild()` after every
edit, and `Canvas::text_key` (`src/ui/canvas.rs:761`) calls it on **every keystroke** of on-canvas typing.

Sequences:

- A 40-layer document scrolled to the bottom. Paint one brush stroke → `finish_stroke` → `refresh` →
  the panel jumps to the top and you have to scroll down again. Repeat per stroke.
- Double-click a layer name to rename it in place (`layers.rs:369-380`), type half a new name, then use
  any menu command or press Ctrl+Z. `edit()` → `refresh()` → `rebuild()` destroys the entry and the typed
  text is gone with no warning. (Focus-leave cancelling the rename, `layers.rs:378`, is intended; an
  unrelated edit cancelling it is not.)

Confidence: confirmed by tracing.

### 21. [P3] A cancelled layer drag leaves a `DocRef` pinned in a thread-local

`src/ui/layers.rs:22`, `447-451`, `470`.

```rust
thread_local! { static DRAG: RefCell<Option<(DocRef, Vec<Uuid>)>> = const { RefCell::new(None) }; }
```

`connect_prepare` fills it; only `connect_drop` (`layers.rs:470`) takes it back out. A drag released
outside any row, over another application, or aborted with Escape never reaches `connect_drop`.

Sequence: start dragging a layer row, drop it on the canvas or outside the window. `DRAG` keeps the
`Rc<RefCell<Doc>>` and the ids. Close that tab: the document is kept alive by the thread-local (on top of
finding 5). The stale ids are also what the *next* drop would read if a source ever failed to run
`connect_prepare`.

Confidence: confirmed by tracing.

### 22. [P3] The Compy mark in the header does not repaint on a theme change

`src/ui/icons.rs:19-29`, `src/ui/mod.rs:379-382`, `src/ui/theme.rs:278-296`.

Every other glyph goes through `glyph_area` (`src/ui/icons.rs:33-47`), which reads `area.color()` — a CSS
property, so GTK's style revalidation queues the redraw for free. `compy_mark` instead reads
`super::theme::accent()` from a thread-local that CSS knows nothing about, and the theme watcher's
`on_change` only touches canvases:

```rust
theme::start(Rc::new(move || { if let Some(state) = weak.upgrade() { for page in state.pages.borrow().iter() { page.canvas.drop_cache(); } } }));
```

Sequence: run `omarchy theme set <other>` where the two themes share a foreground colour but differ in
accent. The rail icons, canvas handles and chrome follow; the mark in the middle of the header keeps the
old accent until something else forces the header to redraw. With no document open, `state.pages` is
empty and the callback does nothing at all.

Confidence: plausible (depends on whether GTK's style invalidation happens to touch that widget for the
particular pair of palettes).

### 23. [P3] One trailing comment in `colors.toml` disables the whole theme

`src/ui/theme.rs:58-82`

```rust
let Some((key, value)) = line.split_once('=') else { continue };
map.insert(key.trim(), value.trim().trim_matches('"'));
let get = |key: &str| map.get(key).filter(|v| hex(v).is_some()).map(|v| v.to_string());
let background = get("background")?;
let foreground = get("foreground")?;
```

A line such as `background = "#08131c"  # base` parses to the value `#08131c" # base`, which `hex` rejects
(length != 6), so `get("background")` is `None` and the whole `parse` returns `None`. `reload` then calls
`apply(None)` and the app silently drops to the stock GTK look — the same outcome as "no Omarchy on this
machine", with nothing printed. The same happens if a hand-written theme omits either key, or if a value is
a named colour. The file that ships today (`~/.local/state/omarchy/current/theme/colors.toml`) has no
trailing comments, so this only bites custom themes.

A second, smaller gap: `apply(None)` removes the CSS provider but never resets
`gtk_application_prefer_dark_theme`, which is only ever *set* (`theme.rs:264-266`).

Confidence: confirmed by tracing.

### 24. [P3] `on_accent` is probably inverted for a light palette

`src/ui/theme.rs:88`

```rust
let on_accent = if p.dark { dbg } else { bfg };
```

`bright_foreground` is the *most contrasting foreground*, i.e. near-black in a light palette. It is then
used as the text/mark colour on top of the accent **background** at
`@define-color accent_fg_color`, `check:checked`, `radio:checked`, `popover listview > row:selected`
and `selection` (theme.rs:92, 150-154, 158). A light Omarchy theme with a mid-to-dark saturated accent
(a blue, a purple) therefore draws near-black on near-black: a ticked check box and a selected popover
row become unreadable. The dark branch (`dark_background` on a bright accent) is right. A light palette
wants the background, not the darkest foreground.

`mode` is also assumed dark when absent: `map.get("mode").is_none_or(|m| *m != "light")` (theme.rs:71), so
a theme that omits the key gets the dark branch regardless of its actual colours.

Confidence: plausible (no light `colors.toml` on this machine to check against).

### 25. [P3] Dead condition in the History window's first row

`src/ui/dialogs.rs:268`

```rust
list.append(&row("Open", past.is_empty() && false));
```

`&& false` makes `dim` unconditionally false. Harmless, but it is either a leftover or a lost intent (the
"Open" row was presumably meant to dim when it is not the current step).

---

## Checked and found sound

- **Borrow discipline in the sync paths.** `OptionsBar::sync_move`, `TypePage::show` and
  `LayersPanel::sync_controls` all set their `syncing` flag *before* the first `set_value` /
  `set_sensitive` that can re-enter, so the write-back handlers bail before taking `borrow_mut`
  (`tools.rs:594-596`, `tools.rs:262-274`, `layers.rs:491-508`). Every options-bar and panel control uses
  `try_borrow_mut` rather than `borrow_mut`. `Canvas::sync_inspector` drops its shared borrow before
  calling into `options`. The marching-ants timer uses `try_borrow_mut` (`canvas.rs:270`).
- **`Document::apply` keeps `active` valid** (`document.rs:124-130`), so `geometry()`, `draw_overlays`,
  `connect_move_fields`, `sync_move` and `rename_layer` cannot index a missing layer after undo. Only the
  drag-captured ids in finding 1 are unguarded.
- **Layer Style teardown.** `window.connect_close_request` → `end(true)` (`effects.rs:80`) and the `ended`
  one-shot cell mean a window-manager close behaves exactly like OK; `effects_preview` is never left on.
  `begin_layer_style`, `preview_effects` and `end_layer_style` all guard with `has_layer`
  (`document.rs`), so deleting the layer under an open dialog degrades instead of panicking.
- **`with_current` with no current tab** returns without calling the closure (`mod.rs:1131-1136`), and
  `edit` / `with_doc` / `current_*` all tolerate that.
- **`open_path` failure leaves nothing behind**: `open_document` errors before `add_page`, so a directory,
  a corrupt PSD or an undecodable image never creates a tab (`mod.rs:1036-1052`).
- **Recent files self-prune**: `recent::list` filters on `p.exists()` and `remember` rewrites from the
  filtered list (`recent.rs:14-28`), so a deleted file drops out on its own.
- **Export Sizes with nothing ticked** reports "Tick at least one size." and does not run
  (`dialogs.rs:236-241`); no folder chosen with files (not artboards) is caught too.
- **Unsaved prompt**: Cancel (index 2) is ignored correctly (`dialogs.rs:399`).
- **Drop onto itself** is rejected in `Document::move_layers` (`moving.contains(&anchor)`), as are a folder
  into itself and a non-folder parent, and `validate::hierarchy` runs before the swap.
- **Cross-panel drops** are guarded by `Rc::ptr_eq` before taking both borrows (`layers.rs:473-480`), so
  dropping a row back onto its own panel cannot double-borrow.
- **Rename to empty** is rejected in `Document::rename_layer` (name trimmed, empty and >16 KB refused).
- **Opacity entry on a group** is desensitised via `sync_controls`'s `editable` flag, and the resulting
  focus-leave is swallowed by the `syncing` flag set one line earlier.
- **Thumbnails are cached** by `(id, size)` (`render/mod.rs` `thumbnail`) and go through `halved_copy`, so a
  30 000 px layer costs one reduction, not a full-size resample, per panel rebuild.
- **Accelerators**: all 119 action accelerator strings plus the five `win.filter::*` ones parse as valid
  GTK accelerators, and no two actions share one (checked by extraction and `uniq -d`). GTK4 runs
  application (global-scope) shortcuts after the focus widget, so `Ctrl+A`, `Delete`, `Ctrl+C`/`V` and the
  arrow keys inside a `GtkText` reach the entry, not the menu; `mod.rs:392-399` additionally short-circuits
  the tool letters while an Editable has focus.
- **Text caret**: there is no blink timer at all (the caret is drawn statically in `draw_overlays`,
  `canvas.rs:2288-2322`), so nothing can leak when the editor closes; `sync_inspector` and `text_key` both
  drop the edit when the layer goes (`canvas.rs:642-643`, `728-729`).
- **Cursor names** used in `update_cursor` and the drag handlers (`grab`, `grabbing`, `zoom-in`,
  `crosshair`, `text`, `move`, `alias`, `none`, `default`, `ns-/ew-/nwse-/nesw-resize`, `pointer`) are all
  CSS cursor names GTK4 resolves on Wayland from the cursor theme; none are X11-only names.
- **Partial redraw** clips to the dirty rectangle converted through the viewport with a 2 px margin
  (`canvas.rs:185-193`), and `cached_frame` rebuilds from scratch whenever width, height, scale or viewport
  change, so a fractional zoom cannot leave a half-updated frame.
- **`take_dirty` ordering** in the draw func: the GPU-present branch consumes it (`canvas.rs:229`) and the
  CPU branch consumes it inside `cached_frame`; the two cannot both run for one frame.
