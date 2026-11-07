# TopVNC mobile control wireframe

Interactive design prototype only. No live hosts, passwords, or remote input.

## Run locally

From this directory:

```powershell
python -m http.server 4178 --bind 127.0.0.1 --directory dist
```

Open <http://127.0.0.1:4178/>. Select Portrait or Landscape; use **Try full
viewport** on a phone to try the layout at its actual viewport size.

## Review flows

- **Type:** the custom TopVNC keyboard is the default. Tap character keys, use
  Shift, symbols, Space, or hold Backspace; Insert puts the phrase in the demo
  remote editor. Four thumb-dock controls keep their order across orientations.
- **Languages:** EN, Thai, Chinese, and Japanese are editable examples. The +
  panel adds/removes language profiles and pins up to four. Edit each profile's
  three character rows. Preferences are saved only in this browser.
- **Chinese/Japanese:** type `nihao` or `zhongwen` in Chinese, or `nihon` or
  `arigatou` in Japanese, then select a candidate and Insert. Candidates are
  explicitly labeled demo fixtures; no complete conversion engine is included.
- **Native input:** switches to a real editable field labeled with the detected
  OS. Your browser and OS control whether an on-screen keyboard opens. The
  website does not switch OS languages; use the native keyboard's globe key.
- **Keys:** tap modifiers for the next key, configure four pinned shortcuts, or
  open the Fn grid. Mode/rotation/focus changes release modifiers.
- **Pointer:** drag in the demo trackpad to move the sample cursor.

In landscape, Type uses a wide split keyboard below the remote display. Keys
and Pointer use a side deck. Character keys use compact keyboard spacing; the
primary dock controls remain at least 44 CSS pixels high.

## Verification limits

JavaScript syntax, HTML element references, and local HTTP assets are checked.
Browser automation was unavailable: the browser host failed, and the Windows
fallback stopped because it could not verify the active URL. Visual rendering,
real-device thumb reach, native keyboard visibility, and IME behavior need
manual validation. This prototype makes no latency or platform-support claim.

Source lives here. `.sites-runtime/` is an ignored, separate Sites publication
checkout and package area; it is not part of the application's Git history.
