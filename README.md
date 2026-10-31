<p align="center">
  <img src="assets/logo.svg" alt="qalam — قلم" width="560">
</p>

<p align="center">
  <b>Phone-style typing smarts for every Windows app.</b><br>
  Word suggestions · autocorrect · spell-check squiggles · text replacement · English &amp; العربية · Arabizi
</p>

---

System-wide word suggestions, autocorrect, spell-check underlines and text replacement for Windows, in Rust.
One exe: install it for your user, or run it portable.

## First run

Launch `qalam.exe`. It asks:

- **Install for this user**: copies to `%LOCALAPPDATA%\Programs\qalam`, adds a Start Menu shortcut,
  starts with Windows, and shows up in Settings > Apps (uninstall there). No admin needed.
- **Run portable**: makes a `qalam-data` folder next to the exe and keeps all settings there.
- **Just run it this time**.

Turn off Windows' own "Show text suggestions when typing on the physical keyboard" so the two don't fight.

## Use

| Key | Popup open | Popup closed |
|---|---|---|
| Tab | accept highlighted word | normal Tab |
| Up | enter popup; Left/Right pick, Enter/Tab accept, Down leaves | normal |
| Esc | close popup | normal |
| Click a word | accept it | |
| Backspace right after an autocorrect | undo it and remember the word | |
| Right-click an underlined word | fixes + "Add to dictionary" | |

- **Tray icon**: left-click opens Replacements; right-click for Languages, Highlight corrections,
  Underline misspelled words, Paused, Start with Windows, settings file, data folder, Quit.
- **Replacements**: type a trigger, then space/Enter/punctuation. Tick **Secret** for passwords and IDs:
  they're encrypted with your Windows login (DPAPI) and never shown again.
- **Highlight corrections**: the corrected word flashes for 250 ms, then fades out.
- **Underlines**: red squiggles under unknown words in the focused text box. Uses UI Automation, so it works in
  Chrome/Edge, Discord and other Electron apps, Notepad, Word; not in games or most terminals. Apps that already
  underline (e.g. Discord with its spell check on) can go in `underline_skip_apps`.

## Languages

- English and Arabic (العربية). By default every enabled language is used at once, picked by the script you type in.
  "Only the keyboard layout's language" switches to following Win+Space.
- Arabic gets suggestions and underlines; autocorrect for Arabic is off unless you tick **Autocorrect Arabic**
  (Arabic glues prefixes and suffixes onto words, so a word list misses many real words).
  Diacritics are ignored, and hamza/ta marbuta spellings (انا → أنا) are treated as the same word.
- **Arabizi → Arabic** (experimental): type `mar7aba`, `ya3ni`, `7abibi` and the popup offers مرحبا، يعني، حبيبي.
  It never replaces on its own; pick from the popup.

Settings live in `config.toml` (tray > Edit settings file); changes apply within a couple of seconds.

## How it works

- `hooks` thread: low-level keyboard/mouse hooks feed `engine.rs`, which tracks what you typed and decides what to do.
- `sender` thread: types with `SendInput` (events tagged so the hook ignores them).
- `spell` thread: UI Automation reads the visible text of the focused box for underlines and finds the corrected
  word's position for the highlight. A hung app can't freeze typing.
- Main thread: suggestion popup, overlays, tray, Replacements window, focus tracking, password-box detection.
- No suggestions, autocorrect, underlines or learning in password boxes. Replacements still work there.

## Known limits

- After a click, window switch, arrow keys or paste, qalam re-reads the text left of the caret (UI Automation) so a
  half-typed word keeps working. In apps without UI Automation text it waits for the next space instead.
- Underlines are re-checked ~0.3 s after typing or scrolling, so they briefly lag behind fast scrolling.
- Windows running as admin don't get hooked unless qalam also runs as admin.

## Build

```
cargo test
cargo build --release
```

## Releases

Download `qalam.exe` from [Releases](https://github.com/watdafawx/qalam/releases). It's a single exe: run it and pick Install or Portable.
Windows SmartScreen may warn because the exe isn't code-signed.

To cut a release: bump `version` in `Cargo.toml`, then `git tag v0.x.y && git push --tags`. GitHub Actions builds and publishes it.

## Credits & license

- Code: MIT (see [LICENSE](LICENSE)).
- Word frequency lists (`data/en_50k.txt`, `data/ar_50k.txt`) from [FrequencyWords](https://github.com/hermitdave/FrequencyWords)
  by Hermit Dave, built from OpenSubtitles, licensed [CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/).
