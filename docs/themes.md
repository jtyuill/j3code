# Themes

A j3 desktop theme is one JSON file. Its id is the file name without `.json`.

- Built in: `crates/j3-desktop/themes/` (`light`, `dark`, `tokyo-night`, `tokyo-night-storm`, `tokyo-night-moon`, `tokyo-night-day`), compiled into the binary.
- Yours: `<config dir>/j3/themes/*.json` (`~/.config/j3/themes/` on Linux, `~/Library/Application Support/j3/themes/` on macOS). The directory is read at startup and each time Settings opens. A file named like a built-in theme is refused.

Pick a theme in Settings › General › Theme, or set `"theme": "<id>"` in the `"ui"` object of `desktop.json`. Without it, j3 shows `light` or `dark`, following the system. To share a theme, send the file.

## Write one

Three colours are enough. Everything else (surfaces, borders, hover, secondary text, shadows) is derived from them.

```json
{
  "name": "My Theme",
  "appearance": "dark",
  "background": "#1a1b26",
  "foreground": "#c0caf5",
  "accent": "#7aa2f7"
}
```

Then check it and look at it, without touching your settings:

```sh
j3 --check-theme my-theme.json
J3_THEME=my-theme.json j3 --snap /tmp/thread.png --scene thread --size 1280x820
J3_THEME=my-theme.json j3            # the app itself, in that theme
```

`--check-theme` takes any number of files, prints `file: ok (Name, dark)` or `file: <what is wrong>` for each, and exits 1 if one fails. Unknown keys are errors, and the message lists the keys that section takes. `J3_THEME` takes a file or an id and overrides the setting for that run. `j3 --list-scenes` lists the scenes; `settings`, `terminal`, `files` and `git-commit` cover most surfaces.

Copy `crates/j3-desktop/themes/tokyo-night.json` for a complete example.

## Format

Colours are sRGB hex: `#rgb`, `#rrggbb` or `#rrggbbaa`.

| Key | Required | Value |
|---|---|---|
| `name` | yes | The name shown in Settings. |
| `appearance` | yes | `"light"` or `"dark"`. |
| `background`, `foreground`, `accent` | all three or none | Opaque colours the palette is derived from. `foreground` must be lighter than `background` in a dark theme and darker in a light one. Without them the theme starts from j3's own Light or Dark palette. |
| `status` | no | `success`, `warning`, `danger`, `info`: one colour each, for text and icons. The role's fill and border are mixed from it. |
| `tokens` | no | Single palette tokens, applied last. |
| `syntax` | no | Code colours: `keyword`, `string`, `number`, `comment`, `type`, `function`, `property`, `punctuation`, `parameter`, `inserted`, `deleted`. Kinds left out keep a hue derived from the text colour. |
| `terminal` | no | `{"ansi": [...]}`: 16 colours, black, red, green, yellow, blue, magenta, cyan, white, then their bright versions. Left out, the terminal uses Light's or Dark's. Its foreground, background and cursor are `text_base` and `bg_base`. |

### Tokens

The names are the fields of rune-kit's `Palette`. The ones a theme usually sets:

| Token | Used for |
|---|---|
| `bg_base` | Panes, cards, the terminal's background. Starts as `background`. |
| `bg_deep`, `page` | The window behind the panes (the sidebar, the version bar). |
| `layer_01` … `layer_04` | Raised surfaces, one step each: popovers, fields, selected rows. |
| `text_base`, `text_muted`, `text_faint` | Text, secondary text, placeholders and line numbers. |
| `icon_base`, `icon_muted`, `icon_faint` | The same three steps for icons. |
| `text_accent`, `icon_accent`, `bg_accent` | Links, accent marks, selected chips. Start as `accent`. |
| `bg_contrast`, `text_contrast` | Primary buttons and their labels. |
| `border_muted`, `border_base`, `border_strong` | Hairlines, card and field borders. |
| `border_focus` | The focus ring. |
| `hover`, `pressed` | Translucent overlays under the pointer. |
| `scrim` | Behind dialogs. |
| `success_fg`, `success_bg`, `success_border`, `success_icon` | One part of a status role; likewise `warning_`, `danger_`, `info_`. |

The rest: `bg_inverse`, `bg_button_neutral`, `bg_icon_button_contrast`, `submit_highlight`, `submit_hover`, `submit_fg`, `submit_icon`, `text_inverse`, `icon_inverse`, `icon_contrast`, `contrast_hover`, `contrast_pressed`, `depth_top`, `depth_bottom`, `white`, `black`, `knob`, `knob_on`, `surface_base`, `surface_weak`, `surface_hover`, `surface_raised`, `surface_raised_base`, `surface_raised_hover`, `surface_float`, `surface_disabled`, `border_weak`, `border_hover`, `border_active`, `border_disabled`, `text_strong`, `text_weak`, `text_invert_strong`, `icon_weaker`, `std_border`, `std_icon`, `std_icon_invert`, `std_icon_strong`, `std_text`.

A theme sets colours only. Fonts, sizes and zoom are settings; radii and motion are j3's.

## Where it lives in the code

- `crates/j3-desktop/src/theme/file.rs`: the format, its checks, and building the kit.
- `crates/j3-desktop/src/theme/library.rs`: the built-in and user themes, `J3_THEME`.
- `crates/j3-desktop/src/theme.rs`: the kit on screen (`theme::kit`), its code and terminal colours.

A new built-in theme is a file in `crates/j3-desktop/themes/` and a line in `library::BUILTIN`.
