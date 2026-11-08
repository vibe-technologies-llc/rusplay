---
paths:
  - "crates/rusplay-app/**"
---

# Desktop app visual design

The app is meant to read like an instrument (OBS, a DAW, a grading tool), not like a SaaS
template: flat graphite surfaces, one accent used sparingly, hairlines instead of shadows, state
written as words, and numbers that hold still.

- **Palette** (`theme.rs`, one function per role, never a literal colour in a view): `canvas`
  (page), `chrome` (titlebar, status bar, sidebars, details panel), `surface` (panels), `raised`
  (hover, selected rows and segments, secondary buttons), `inset` (inputs, tracks), `rule` and
  `rule_strong` (hairlines, control borders), `text`, `text_muted`, `text_faint`. The neutrals are
  warm graphite with no blue tint. Status colours are `live`, `warning` and `danger`.
- **One accent, copper** (`accent`, `accent_hover`, with `accent_ink` as the text on it). It
  marks only the primary action, a selection bar, a toggle that is on, focus, the caret and an
  override. `accent_quiet` is the fill of a chosen option button and the base of the text
  selection colour. Selected rows use `raised` with the 2px accent `selection_bar`, not a tinted
  fill.
- **No gradients, glows or card shadows.** Panels are `surface` with a 1px `rule` border and a 6px
  radius; controls have a 4px radius. Only two things cast a shadow: the floating window
  (`window_shadow`) and the dropdown list, the one layer that floats over the page.
- **No logo tile, avatars or taglines.** The titlebar shows the wordmark. Page tabs are underline
  tabs.
- **Sentence case everywhere.** Group headings are `group_heading` (muted, semibold, no rule) and
  never uppercase. The fullscreen overlay is the exception, since its bitmap font only has
  capitals.
- **State is a word, not a pill.** `state_text(label, Tone)` is coloured text; `status_dot` is a
  plain 6px dot used only for a live state (stream header, host status). At most one state word
  per row.
- **Headings carry an action, not a subtitle.** `panel_heading(title, action)` puts a button or a
  count on the right. A line under a heading is there only when it says something the controls
  do not (the per-host override explanation).
- **Empty states are one left-aligned muted line**, not a centred box.
- **Numbers use tabular figures** (`.font_features(theme::tabular())`) wherever they update or
  line up. Units come from the shared helpers in `stats.rs`: `millis_text` (ms), `fps_text`,
  `rate_text` (/s) and `bitrate_text` (Mbit/s), one decimal.
- **Layouts do not jump as data arrives.** The connection details panel always shows the same
  sections and rows (`stream/readout.rs`); a value not known yet is a faint `waiting`, and once
  rates exist a missing presentation feedback says `no feedback`. Non-zero loss, decode-error
  and underrun rates are `warning`.
- **Disabled controls**: a disabled primary button turns neutral (`raised`, `text_faint`)
  instead of a dimmed accent; other buttons fade to 40%.
- **Type scale**: `text_lg` for page and section titles, `text_sm` for body and controls,
  `text_xs` for meta, labels in the details panel and the status bar.
- **Contrast is checked**: `theme::tests` requires 4.5:1 for `text` and `text_muted` on every
  surface, for `text_faint` on `canvas` and `chrome` (where it is used), for the status colours
  and the accent on the panels, and for `accent_ink` on the accent. A palette change that breaks
  it fails the test.
- **Checking the look**: run the app in a nested headless KWin (`dbus-run-session`, then
  `kwin_wayland --virtual --socket <name>`) and capture with `spectacle -b -n -f -o <file>` on
  that bus. Set `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_STATE_HOME` and `XDG_CACHE_HOME` to
  **absolute** temporary directories: a relative one is ignored and KWin, spectacle and the app
  then write to the real `~/.config`. A seeded host book (`peers.client_file` in a temporary
  `client.toml`, with the book's directory `0700` and the book `0600`) gives the Hosts page
  entries, and `--connect <name>` selects one.
